package com.getdre.jdbc;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.DeserializationFeature;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import java.io.*;
import java.math.RoundingMode;
import java.net.URL;
import java.net.URLClassLoader;
import java.nio.file.Files;
import java.nio.file.Path;
import java.sql.*;
import java.util.*;
import java.util.concurrent.*;
import java.util.concurrent.atomic.AtomicBoolean;
import org.apache.arrow.adapter.jdbc.*;
import org.apache.arrow.memory.RootAllocator;
import org.apache.arrow.vector.VectorSchemaRoot;
import org.apache.arrow.vector.ipc.ArrowStreamWriter;
import org.apache.arrow.vector.types.pojo.Schema;

/** A JDBC session served over DRE's existing framed subprocess protocol. */
public final class Main {
  private static final ObjectMapper JSON = new ObjectMapper().enable(DeserializationFeature.USE_BIG_DECIMAL_FOR_FLOATS);
  private static final int MAX_FRAME = 1 << 30;
  private final DataInputStream input = new DataInputStream(new BufferedInputStream(System.in));
  private final DataOutputStream output = new DataOutputStream(new BufferedOutputStream(System.out));
  private final ExecutorService worker = daemonExecutor("jdbc-request");
  private final ExecutorService canceller = daemonExecutor("jdbc-cancel");
  private final ConcurrentHashMap<Long, Request> requests = new ConcurrentHashMap<>();
  private volatile Connection connection;
  private URLClassLoader drivers;

  private static final class Request {
    final JsonNode message;
    final AtomicBoolean cancelled = new AtomicBoolean();
    volatile Statement statement;

    Request(JsonNode message) {
      this.message = message;
    }
  }

  private static final class Failure extends Exception {
    private static final long serialVersionUID = 1L;
    final String kind;

    Failure(String kind, String message) {
      super(message);
      this.kind = kind;
    }
  }

  private static ExecutorService daemonExecutor(String name) {
    return Executors.newSingleThreadExecutor(action -> {
      Thread thread = new Thread(action, name);
      thread.setDaemon(true);
      thread.setUncaughtExceptionHandler((failed, error) -> System.exit(2));
      return thread;
    });
  }

  public static void main(String[] args) {
    // A timezone-free JDBC timestamp keeps its wall clock without borrowing the host's zone.
    TimeZone.setDefault(TimeZone.getTimeZone("UTC"));
    Main plugin = new Main();
    // Driver output may contain credentials, including pieces parsed out of a URL.
    // Keep only our framed, categorized diagnostics on the original protocol stream.
    System.setOut(new PrintStream(OutputStream.nullOutputStream()));
    System.setErr(new PrintStream(OutputStream.nullOutputStream()));
    int status = 0;
    try {
      plugin.serve();
    } catch (Exception error) {
      status = 2;
    } finally {
      plugin.requests.values().forEach(plugin::cancel);
      plugin.worker.shutdownNow();
      plugin.canceller.shutdown();
      if (plugin.requests.isEmpty()) {
        try {
          plugin.closeSession();
        } catch (Exception ignored) {
          status = 2;
        }
      }
    }
    System.exit(status);
  }

  private void serve() throws Exception {
    JsonNode hello = read();
    if (hello == null || !hello.path("type").asText().equals("hello")) {
      throw new IOException("expected hello");
    }
    int min = hello.path("min_version").asInt(0);
    int max = hello.path("max_version").asInt(1);
    if (min > max || min > 1 || max < 0) {
      send(null, "version_mismatch", "min_version", 0, "max_version", 1);
      throw new IOException("unsupported protocol");
    }
    if (hello.has("plugin") && !hello.path("plugin").asText().equals("source/jdbc")) {
      send(null, "error", "kind", "plugin", "code", "jdbc/unknown-plugin", "message", "this package provides source/jdbc");
      throw new IOException("unknown plugin");
    }
    send(null, "hello", "protocol_version", Math.min(max, 1), "kind", "source", "name", "jdbc",
        "version", Main.class.getPackage().getImplementationVersion(), "capabilities", List.of("sessions", "validate"), "provides", List.of("source/jdbc"));
    JsonNode message;
    while ((message = read()) != null) {
      long id = message.path("id").asLong(0);
      if (message.path("type").asText().equals("cancel")) {
        Request request = requests.get(id);
        if (request != null) {
          cancel(request);
        }
      } else {
        Request request = new Request(message);
        requests.put(id, request);
        worker.execute(() -> handle(request, id));
      }
    }
  }

  private void cancel(Request request) {
    request.cancelled.set(true);
    canceller.submit(() -> {
      synchronized (request) {
        Statement statement = request.statement;
        if (statement != null) {
          try {
            statement.cancel();
          } catch (SQLException ignored) {
            // The request still observes cancellation between batches and before its reply.
          }
        }
      }
    });
  }

  private JsonNode read() throws IOException {
    int first = input.read();
    if (first < 0) {
      return null;
    }
    int length = (first << 24) | (input.readUnsignedByte() << 16) | (input.readUnsignedByte() << 8) | input.readUnsignedByte();
    if (length < 2 || length > MAX_FRAME || input.readUnsignedByte() != 'J') {
      throw new IOException("invalid control frame");
    }
    byte[] bytes = new byte[length - 1];
    input.readFully(bytes);
    JsonNode node = JSON.readTree(bytes);
    if (node == null || !node.isObject() || !node.path("type").isTextual()) {
      throw new IOException("invalid request");
    }
    return node;
  }

  private synchronized void frame(int tag, byte[] body) throws IOException {
    if (body.length >= MAX_FRAME) {
      throw new IOException("result batch exceeds protocol frame limit");
    }
    output.writeInt(body.length + 1);
    output.writeByte(tag);
    output.write(body);
    output.flush();
  }

  private void send(JsonNode request, String type, Object... fields) throws IOException {
    ObjectNode reply = JSON.createObjectNode().put("type", type);
    if (request != null && request.has("id")) {
      reply.set("id", request.get("id"));
    }
    for (int i = 0; i < fields.length; i += 2) {
      reply.set((String) fields[i], JSON.valueToTree(fields[i + 1]));
    }
    frame('J', JSON.writeValueAsBytes(reply));
  }

  private void handle(Request request, long id) {
    JsonNode message = request.message;
    String operation = message.path("type").asText();
    try {
      checkCancelled(request);
      switch (operation) {
        case "describe" -> describe(message);
        case "validate" -> {
          List<String> errors = new ArrayList<>();
          message.path("options").fieldNames().forEachRemaining(key -> errors.add("unknown option `" + key + "`"));
          send(message, "validated", "errors", errors);
        }
        case "validate_connection" -> validate(message);
        case "open" -> {
          open(message);
          checkCancelled(request);
          send(message, "ok");
        }
        case "execute" -> execute(request);
        case "close" -> {
          closeSession();
          send(message, "ok");
          System.exit(0);
        }
        default -> throw new Failure("plugin", "request is not supported by source/jdbc");
      }
    } catch (Exception | LinkageError | ServiceConfigurationError error) {
      if (operation.equals("open")) {
        try {
          closeSession();
        } catch (Exception ignored) {
          // Preserve the original connection failure; diagnostics never include driver text.
        }
      }
      try {
        String kind;
        String text;
        if (request.cancelled.get()) {
          kind = "cancelled";
          text = "JDBC request cancelled";
        } else if (error instanceof Failure failure) {
          kind = failure.kind;
          text = failure.getMessage();
        } else if (error instanceof SQLException sql) {
          String state = sql.getSQLState();
          state = state != null && state.matches("[A-Z0-9]{5}") ? state : "unknown";
          kind = state.startsWith("28") ? "auth" : operation.equals("open") ? "connection" : "query";
          text = "JDBC " + kind + " failed (SQLSTATE " + state + ", vendor code " + sql.getErrorCode()
              + "); check " + (operation.equals("open") ? "the URL, driver properties, network and database permissions" : "the statement, declared column types and database permissions");
        } else {
          kind = operation.equals("execute") ? "query" : "connection";
          text = operation.equals("execute")
              ? "JDBC result conversion failed; cast unsupported columns to supported JDBC types"
              : "JDBC connection failed; check driver setup and connection configuration";
        }
        send(message, "error", "kind", kind, "code", "jdbc/" + kind + "-failed", "message", text);
      } catch (IOException brokenPipe) {
        System.exit(2);
      }
    } finally {
      requests.remove(id, request);
    }
  }

  private static Map<String, Object> field(String name, String description, boolean required, boolean secret, String kind) {
    Map<String, Object> field = new LinkedHashMap<>();
    field.put("name", name);
    field.put("description", description);
    field.put("required", required);
    field.put("secret", secret);
    if (kind != null) {
      field.put("kind", kind);
    }
    return field;
  }

  private void describe(JsonNode request) throws IOException {
    send(request, "describe", "connection_fields", List.of(
        field("url", "complete JDBC URL or minimal vendor URL; may contain credentials", true, true, "string"),
        field("driver_jars", "local driver JAR path, or a list of paths; vendor drivers are user-supplied", true, false, null),
        field("driver_class", "optional fully qualified JDBC driver class instead of service discovery", false, false, "string"),
        field("properties", "arbitrary case-sensitive vendor settings with string, boolean or numeric values", false, true, "map")),
        "option_fields", List.of(), "identifier_quote", "\"");
  }

  private static List<Path> driverPaths(JsonNode value) {
    List<Path> paths = new ArrayList<>();
    Iterable<JsonNode> entries = value.isArray() ? value : List.of(value);
    for (JsonNode entry : entries) {
      if (!entry.isTextual() || entry.asText().isBlank()) {
        return List.of();
      }
      String path = entry.asText();
      if (path.startsWith("~/")) {
        path = System.getProperty("user.home") + path.substring(1);
      }
      try {
        paths.add(Path.of(path));
      } catch (java.nio.file.InvalidPathException invalid) {
        return List.of();
      }
    }
    return paths;
  }

  private static List<String> connectionErrors(JsonNode connection, Set<String> unresolved) {
    List<String> errors = new ArrayList<>();
    if (!connection.isObject()) {
      return List.of("`connection` must be a block of settings");
    }
    if (!unresolved.contains("url") && (!connection.path("url").isTextual()
        || !connection.path("url").asText().matches("jdbc:[A-Za-z][A-Za-z0-9._-]*:.+"))) {
      errors.add("`url` must be a JDBC URL beginning with jdbc:<vendor>:");
    }
    if (!unresolved.contains("driver_jars")) {
      List<Path> paths = driverPaths(connection.path("driver_jars"));
      if (paths.isEmpty() || paths.stream().anyMatch(path -> !Files.isRegularFile(path) || !Files.isReadable(path))) {
        errors.add("`driver_jars` must name readable local driver JAR files");
      }
    }
    if (connection.has("driver_class") && !unresolved.contains("driver_class")
        && (!connection.path("driver_class").isTextual() || connection.path("driver_class").asText().isBlank())) {
      errors.add("`driver_class` must be a nonempty class name");
    }
    if (connection.has("properties") && !unresolved.contains("properties")) {
      JsonNode properties = connection.path("properties");
      if (!properties.isObject()) {
        errors.add("`properties` must be a block of settings");
      } else {
        properties.properties().forEach(entry -> {
          JsonNode value = entry.getValue();
          if (!value.isTextual() && !value.isBoolean() && !value.isNumber()) {
            errors.add("`properties." + entry.getKey() + "` must be a string, boolean or number");
          }
        });
      }
    }
    return errors;
  }

  private void validate(JsonNode request) throws IOException {
    Set<String> unresolved = new HashSet<>();
    request.path("unresolved").forEach(value -> unresolved.add(value.asText()));
    List<String> warnings = new ArrayList<>();
    Set<String> known = Set.of("url", "driver_jars", "driver_class", "properties", "threads");
    request.path("connection").fieldNames().forEachRemaining(key -> {
      if (!known.contains(key)) {
        warnings.add("unknown key `" + key + "` for source `jdbc`, ignored");
      }
    });
    send(request, "validated", "errors", connectionErrors(request.path("connection"), unresolved), "warnings", warnings);
  }

  private void open(JsonNode request) throws Exception {
    JsonNode config = request.path("connection");
    List<String> errors = connectionErrors(config, Set.of());
    if (!errors.isEmpty()) {
      throw new Failure("config", String.join("; ", errors));
    }
    if (request.path("read_only").asBoolean(false)) {
      throw new Failure("refused", "JDBC does not provide restricted-query sessions; use database permissions for report access");
    }
    closeSession();
    List<URL> urls = new ArrayList<>();
    for (Path path : driverPaths(config.path("driver_jars"))) {
      urls.add(path.toUri().toURL());
    }
    drivers = new URLClassLoader(urls.toArray(URL[]::new), Main.class.getClassLoader());
    Properties properties = new Properties();
    config.path("properties").properties().forEach(entry -> {
      JsonNode value = entry.getValue();
      properties.setProperty(entry.getKey(), value.isFloatingPointNumber() ? value.decimalValue().toPlainString() : value.asText());
    });
    String url = config.path("url").asText();
    List<Driver> candidates = new ArrayList<>();
    if (config.has("driver_class")) {
      Object instance = Class.forName(config.path("driver_class").asText(), true, drivers).getDeclaredConstructor().newInstance();
      if (!(instance instanceof Driver driver)) {
        throw new Failure("config", "`driver_class` must implement java.sql.Driver");
      }
      candidates.add(driver);
    } else {
      ServiceLoader.load(Driver.class, drivers).forEach(candidates::add);
    }
    for (Driver driver : candidates) {
      if (driver.acceptsURL(url)) {
        connection = driver.connect(url, properties);
        if (connection != null) {
          connection.setAutoCommit(true);
          return;
        }
      }
    }
    throw new Failure("config", "no supplied JDBC driver accepts `url`; check `driver_jars` and `driver_class`");
  }

  private void closeSession() throws Exception {
    try {
      if (connection != null) {
        connection.close();
      }
    } finally {
      connection = null;
      if (drivers != null) {
        drivers.close();
        drivers = null;
      }
    }
  }

  private static void checkCancelled(Request request) throws Failure {
    if (request.cancelled.get()) {
      throw new Failure("cancelled", "JDBC request cancelled");
    }
  }

  private void execute(Request request) throws Exception {
    if (connection == null) {
      throw new Failure("connection", "open a JDBC connection before executing SQL");
    }
    JsonNode message = request.message;
    if (!message.path("sql").isTextual()) {
      throw new Failure("config", "`sql` must be a statement string");
    }
    JsonNode limit = message.get("row_limit");
    if (limit != null && (!limit.isIntegralNumber() || !limit.canConvertToLong() || limit.asLong() < 0)) {
      throw new Failure("config", "`row_limit` must be a nonnegative whole number");
    }
    long remaining = limit == null ? Long.MAX_VALUE : limit.asLong();
    ExecutionResult execution;
    try (Statement statement = connection.createStatement()) {
      request.statement = statement;
      checkCancelled(request);
      statement.setFetchSize(8192);
      if (!statement.execute(message.path("sql").asText())) {
        checkCancelled(request);
        execution = ExecutionResult.noResult(Math.max(0, statement.getLargeUpdateCount()));
      } else try (ResultSet result = statement.getResultSet(); RootAllocator allocator = new RootAllocator(512L << 20)) {
        ResultSetMetaData metadata = result.getMetaData();
        boolean oracle = connection.getMetaData().getDatabaseProductName().equalsIgnoreCase("Oracle");
        List<String> columns = new ArrayList<>();
        for (int column = 1; column <= metadata.getColumnCount(); column++) {
          columns.add(metadata.getColumnLabel(column));
          int type = metadata.getColumnType(column);
          if (oracle && type == Types.DATE) {
            throw new Failure("query", "Oracle DATE can contain time-of-day; cast to TIMESTAMP(3) or exact text instead of the driver's date-only mapping");
          }
          if ((type == Types.NUMERIC || type == Types.DECIMAL)
              && (metadata.getPrecision(column) < 1 || metadata.getPrecision(column) > 38
                  || metadata.getScale(column) < 0 || metadata.getScale(column) > metadata.getPrecision(column))) {
            throw new Failure("query", "JDBC decimal columns need declared precision 1..38 and scale 0..precision; cast the column in SQL");
          }
          if ((type == Types.TIMESTAMP || type == Types.TIME) && metadata.getScale(column) > 3) {
            throw new Failure("query", "JDBC time/timestamp columns need precision at most 3 in this version; cast explicitly to millisecond precision in SQL");
          }
        }
        JdbcToArrowConfig config = new JdbcToArrowConfigBuilder(allocator, null)
            .setTargetBatchSize(8192).setBigDecimalRoundingMode(RoundingMode.UNNECESSARY).build();
        Schema schema = JdbcToArrowUtils.jdbcToArrowSchema(metadata, config);
        send(message, "result", "columns", columns);
        long rows = 0;
        boolean sent = false;
        try (ArrowVectorIterator batches = JdbcToArrow.sqlToArrowVectorIterator(result, config)) {
          while (remaining > 0 && batches.hasNext()) {
            checkCancelled(request);
            try (VectorSchemaRoot batch = batches.next()) {
              int count = (int) Math.min(batch.getRowCount(), remaining);
              try (VectorSchemaRoot slice = batch.slice(0, count)) {
                arrow(slice);
              }
              rows += count;
              remaining -= count;
              sent = true;
            }
          }
        }
        if (!sent) {
          try (VectorSchemaRoot empty = VectorSchemaRoot.create(schema, allocator)) {
            arrow(empty);
          }
        }
        checkCancelled(request);
        execution = ExecutionResult.result(rows);
      }
    } finally {
      // Finish cancellation before another statement can use this database connection.
      synchronized (request) {
        request.statement = null;
      }
    }
    if (execution.resultStarted()) {
      send(message, "result_end", "rows", execution.rows());
    } else {
      send(message, "no_result", "rows_affected", execution.rows());
    }
  }

  private record ExecutionResult(boolean resultStarted, long rows) {
    static ExecutionResult result(long rows) {
      return new ExecutionResult(true, rows);
    }

    static ExecutionResult noResult(long rows) {
      return new ExecutionResult(false, rows);
    }
  }

  private void arrow(VectorSchemaRoot batch) throws IOException {
    ByteArrayOutputStream bytes = new ByteArrayOutputStream();
    try (ArrowStreamWriter stream = new ArrowStreamWriter(batch, null, bytes)) {
      stream.start();
      stream.writeBatch();
      stream.end();
    }
    frame('A', bytes.toByteArray());
  }
}
