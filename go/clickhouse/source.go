package main

import (
	"context"
	"crypto/tls"
	"errors"
	"fmt"
	"io"
	"reflect"
	"strings"
	"sync"
	"time"

	"github.com/ClickHouse/clickhouse-go/v2"
	"github.com/ClickHouse/clickhouse-go/v2/lib/driver"
	"github.com/apache/arrow-go/v18/arrow"
	"github.com/apache/arrow-go/v18/arrow/array"
	"github.com/google/uuid"

	"github.com/get-dre/dre/go/plugin"
)

// batchRows is how many rows go in one Arrow batch.
const batchRows = 65536

// session is one ClickHouse session. Over HTTP every request carries the session's session_id;
// over native the pool holds one connection. Either way temp tables and SET from one statement
// are there for the next.
type session struct {
	conn   driver.Conn
	opts   *clickhouse.Options
	tunnel *tunnel
	native bool
	// stateful is set once a statement may have left state on the session (a temp table, a
	// SET): the driver drops a native connection after an error, and that state with it.
	stateful bool
	lost     error

	mu      sync.Mutex
	cancel  context.CancelFunc
	queryID string
}

func open(conn map[string]any, level int) (*session, error) {
	o, err := options(conn, level)
	if err != nil {
		return nil, &plugin.Error{Kind: "config", Message: err.Error()}
	}
	s := &session{opts: o, native: o.Protocol == clickhouse.Native}
	if b, ok := conn["ssh"].(map[string]any); ok {
		s.tunnel = &tunnel{settings: b, timeout: o.DialTimeout}
		o.DialContext = s.tunnel.dialer(nativeTLS(o))
	}
	if !s.native {
		o.Settings["session_id"] = "dre-" + uuid.NewString()
		if _, ok := o.Settings["session_timeout"]; !ok {
			o.Settings["session_timeout"] = 3600
		}
	}
	retries, err := plugin.Int(conn, "retries", 3)
	if err != nil {
		return nil, err
	}
	s.conn, err = plugin.Retry(int(retries), "connecting to ClickHouse", func() (driver.Conn, error) {
		c, err := clickhouse.Open(o)
		if err == nil {
			ctx, cancel := context.WithTimeout(context.Background(), o.DialTimeout+o.ReadTimeout)
			defer cancel()
			err = c.Ping(ctx)
		}
		if err != nil {
			if c != nil {
				c.Close()
			}
			if plugin.IsConnectionError(err) {
				return nil, &plugin.TemporaryError{Err: err}
			}
			return nil, err
		}
		return c, nil
	})
	if err != nil {
		s.tunnel.closeIfSet()
		if s.tunnel != nil && s.tunnel.err != nil {
			return nil, &plugin.Error{Kind: "connection", Message: fmt.Sprintf("can't open a ClickHouse session at %s through the SSH bastion: %v", o.Addr[0], s.tunnel.err)}
		}
		return nil, openErr(err, o)
	}
	return s, nil
}

// nativeTLS is the TLS a tunnelled native connection needs from the dialer (over HTTP, net/http
// adds it).
func nativeTLS(o *clickhouse.Options) *tls.Config {
	if o.Protocol == clickhouse.Native {
		return o.TLS
	}
	return nil
}

// start makes a query's context: tagged with a query ID, and cancellable by Cancel.
func (s *session) start() (context.Context, func()) {
	id := uuid.NewString()
	ctx, cancel := context.WithCancel(clickhouse.Context(context.Background(), clickhouse.WithQueryID(id)))
	s.mu.Lock()
	s.cancel, s.queryID = cancel, id
	s.mu.Unlock()
	return ctx, func() {
		s.mu.Lock()
		s.cancel, s.queryID = nil, ""
		s.mu.Unlock()
		cancel()
	}
}

// failed records that a statement failed: a native connection is gone, and its state with it.
func (s *session) failed(err error) error {
	if s.native && s.stateful && s.lost == nil {
		s.lost = err
	}
	return cleanErr(err)
}

func (s *session) usable() error {
	if s.lost != nil {
		return errors.New("an earlier statement failed, and ClickHouse's native protocol closes the connection on an error, " +
			"so this session's temp tables and SET are gone; fix that statement, or use `driver: http`, whose sessions survive errors")
	}
	return nil
}

func (s *session) Run(query string, fn func(plugin.Result) error) error {
	if err := s.usable(); err != nil {
		return err
	}
	if !returnsRows(query) {
		// CREATE TEMPORARY TABLE, SET, ...: state on the session. Sent as a statement without a
		// result: over native, asking for rows that don't come drops the connection.
		if err := s.exec(query); err != nil {
			return err
		}
		s.stateful = true
		return fn(nil)
	}
	ctx, done := s.start()
	defer done()
	rows, err := s.conn.Query(ctx, query)
	if err != nil {
		return s.failed(err)
	}
	defer rows.Close()
	types := rows.ColumnTypes()
	r := &result{rows: rows, cols: make([]column, len(types)), dest: make([]any, len(types))}
	fields := make([]arrow.Field, len(types))
	for i, t := range types {
		r.cols[i] = newColumn(t.Name(), t.DatabaseTypeName())
		r.dest[i] = reflect.New(t.ScanType()).Interface()
		fields[i] = arrow.Field{Name: t.Name(), Type: r.cols[i].arrow, Nullable: true}
	}
	r.schema = arrow.NewSchema(fields, nil)
	if err := fn(r); err != nil {
		return err
	}
	if err := rows.Err(); err != nil {
		return s.failed(err)
	}
	return nil
}

// Check runs EXPLAIN, which analyses a SELECT without running it. ClickHouse can only EXPLAIN a
// SELECT; anything else is parsed (EXPLAIN AST), and the note says so.
func (s *session) Check(query string) (string, error) {
	if err := s.usable(); err != nil {
		return "", err
	}
	if returnsRows(query) {
		err := s.drain("EXPLAIN " + query)
		// INCORRECT_QUERY: "Only SELECT is supported for EXPLAIN query" (SHOW, DESCRIBE, ...).
		if code(err) != 80 {
			return "", err
		}
	}
	if err := s.drain("EXPLAIN AST " + query); err != nil {
		return "", err
	}
	return "ClickHouse can only analyse a SELECT without running it; this statement was parsed, not analysed", nil
}

func (s *session) drain(query string) error {
	ctx, done := s.start()
	defer done()
	rows, err := s.conn.Query(ctx, query)
	if err != nil {
		return s.failed(err)
	}
	defer rows.Close()
	for rows.Next() {
	}
	if err := rows.Err(); err != nil {
		return s.failed(err)
	}
	return nil
}

// Load puts the rows into a temporary table on the session, sent as a native insert.
func (s *session) Load(name string, schema *arrow.Schema, recs []arrow.Record) (plugin.Loaded, error) {
	if err := s.usable(); err != nil {
		return plugin.Loaded{}, err
	}
	table := quote("dre_lookup_" + name)
	var defs []string
	for _, f := range schema.Fields() {
		t, err := loadType(f.Type)
		if err != nil {
			return plugin.Loaded{}, err
		}
		defs = append(defs, quote(f.Name)+" Nullable("+t+")")
	}
	if err := s.exec("DROP TEMPORARY TABLE IF EXISTS " + table); err != nil {
		return plugin.Loaded{}, err
	}
	if err := s.exec(fmt.Sprintf("CREATE TEMPORARY TABLE %s (%s)", table, strings.Join(defs, ", "))); err != nil {
		return plugin.Loaded{}, err
	}
	s.stateful = true
	var n int64
	for _, rec := range recs {
		n += rec.NumRows()
	}
	if n == 0 {
		return plugin.Loaded{Relation: table}, nil
	}
	ctx, done := s.start()
	defer done()
	batch, err := s.conn.PrepareBatch(ctx, "INSERT INTO "+table)
	if err != nil {
		return plugin.Loaded{}, s.failed(err)
	}
	for _, rec := range recs {
		for r := 0; r < int(rec.NumRows()); r++ {
			row := make([]any, rec.NumCols())
			for c := range row {
				row[c] = loadValue(rec.Column(c), r)
			}
			if err := batch.Append(row...); err != nil {
				batch.Abort()
				return plugin.Loaded{}, s.failed(err)
			}
		}
	}
	if err := batch.Send(); err != nil {
		return plugin.Loaded{}, s.failed(err)
	}
	return plugin.Loaded{Relation: table, Rows: n}, nil
}

func (s *session) exec(query string) error {
	ctx, done := s.start()
	defer done()
	if err := s.conn.Exec(ctx, query); err != nil {
		return s.failed(err)
	}
	return nil
}

// loadType is the ClickHouse type a lookup column is loaded as.
func loadType(t arrow.DataType) (string, error) {
	switch t.ID() {
	case arrow.STRING:
		return "String", nil
	case arrow.INT64:
		return "Int64", nil
	case arrow.FLOAT64:
		return "Float64", nil
	case arrow.BOOL:
		return "Bool", nil
	case arrow.DATE32:
		return "Date32", nil
	}
	return "", fmt.Errorf("can't load a column of type %s", t)
}

// loadValue is one cell as the driver takes it for a Nullable column.
func loadValue(col arrow.Array, i int) any {
	if col.IsNull(i) {
		return nil
	}
	switch c := col.(type) {
	case *array.String:
		v := c.Value(i)
		return &v
	case *array.Int64:
		v := c.Value(i)
		return &v
	case *array.Float64:
		v := c.Value(i)
		return &v
	case *array.Boolean:
		v := c.Value(i)
		return &v
	case *array.Date32:
		v := c.Value(i).ToTime()
		return &v
	}
	return nil
}

// Cancel stops the running query on the server: cancelling the request alone leaves it running
// there, so it's killed by its query ID over a connection of its own.
func (s *session) Cancel() {
	s.mu.Lock()
	cancel, id := s.cancel, s.queryID
	s.mu.Unlock()
	if cancel == nil {
		return
	}
	cancel()
	o := *s.opts
	o.Settings = clickhouse.Settings{}
	for k, v := range s.opts.Settings {
		if k != "session_id" && k != "session_timeout" && k != "readonly" {
			o.Settings[k] = v
		}
	}
	c, err := clickhouse.Open(&o)
	if err != nil {
		return
	}
	defer c.Close()
	ctx, stop := context.WithTimeout(context.Background(), 10*time.Second)
	defer stop()
	_ = c.Exec(ctx, "KILL QUERY WHERE query_id = {id:String} ASYNC", clickhouse.Named("id", id))
}

func (s *session) Close() {
	if s.conn != nil {
		s.conn.Close()
	}
	s.tunnel.closeIfSet()
}

// result streams one statement's rows as Arrow batches of the declared types.
type result struct {
	rows    driver.Rows
	cols    []column
	dest    []any
	schema  *arrow.Schema
	peeked  bool
	hasNext bool
}

func (r *result) Schema() (*arrow.Schema, error) { return r.schema, nil }

func (r *result) HasNext() bool {
	if !r.peeked {
		r.hasNext, r.peeked = r.rows.Next(), true
	}
	return r.hasNext
}

func (r *result) Next() (arrow.Record, error) {
	if !r.HasNext() {
		if err := r.rows.Err(); err != nil {
			return nil, cleanErr(err)
		}
		return nil, io.EOF
	}
	builders := make([]array.Builder, len(r.cols))
	for i, c := range r.cols {
		builders[i] = c.builder()
		defer builders[i].Release()
	}
	n := int64(0)
	for r.HasNext() && n < batchRows {
		r.peeked = false
		if err := r.rows.Scan(r.dest...); err != nil {
			return nil, cleanErr(err)
		}
		for i, c := range r.cols {
			c.append(builders[i], reflect.ValueOf(r.dest[i]).Elem().Interface())
		}
		n++
	}
	if !r.HasNext() {
		// The rows ended: a failure or a cancel, not the end of the result, sends no partial batch.
		if err := r.rows.Err(); err != nil {
			return nil, cleanErr(err)
		}
	}
	cols := make([]arrow.Array, len(builders))
	for i, b := range builders {
		cols[i] = b.NewArray()
		defer cols[i].Release()
	}
	return array.NewRecord(r.schema, cols, n), nil
}

// returnsRows is whether a statement answers with rows, from its first keyword (after comments
// and opening parentheses). Anything else (DDL, SET, INSERT) is sent without asking for rows.
func returnsRows(query string) bool {
	q := query
	for {
		q = strings.TrimLeft(q, " \t\r\n(")
		switch {
		case strings.HasPrefix(q, "--") || strings.HasPrefix(q, "#"):
			if i := strings.IndexByte(q, '\n'); i >= 0 {
				q = q[i+1:]
				continue
			}
			return false
		case strings.HasPrefix(q, "/*"):
			if i := strings.Index(q, "*/"); i >= 0 {
				q = q[i+2:]
				continue
			}
			return false
		}
		break
	}
	end := strings.IndexFunc(q, func(r rune) bool { return !(r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z') })
	if end < 0 {
		end = len(q)
	}
	switch strings.ToLower(q[:end]) {
	case "select", "with", "show", "desc", "describe", "explain", "exists", "check", "values", "from":
		return true
	}
	return false
}

func quote(ident string) string {
	return "`" + strings.ReplaceAll(strings.ReplaceAll(ident, `\`, `\\`), "`", "\\`") + "`"
}

// code is a ClickHouse error's code, or 0.
func code(err error) int32 {
	var e *clickhouse.Exception
	if errors.As(err, &e) {
		return e.Code
	}
	return 0
}

// cleanErr keeps ClickHouse's message, without the driver's framing, and gives sign-in and
// read-only refusals their protocol kinds.
func cleanErr(err error) error {
	if err == nil {
		return nil
	}
	var e *clickhouse.Exception
	if !errors.As(err, &e) {
		if errors.Is(err, context.Canceled) {
			return &plugin.Error{Kind: "cancelled", Message: "cancelled"}
		}
		return err
	}
	msg := strings.TrimSpace(e.Message)
	if i := strings.Index(msg, " (version "); i > 0 {
		msg = msg[:i]
	}
	msg = strings.TrimSuffix(msg, ".")
	name := e.CodeName
	if name == "" {
		name = e.Name
	}
	if name != "" && !strings.Contains(name, "::") {
		msg = fmt.Sprintf("%s (ClickHouse error %d, %s)", msg, e.Code, name)
	} else {
		msg = fmt.Sprintf("%s (ClickHouse error %d)", msg, e.Code)
	}
	switch e.Code {
	case 516, 192, 193, 194: // AUTHENTICATION_FAILED, UNKNOWN_USER, WRONG_PASSWORD, REQUIRED_PASSWORD
		return &plugin.Error{Kind: "auth", Message: msg}
	case 164: // READONLY
		return &plugin.Error{Kind: "refused", Message: msg}
	}
	return errors.New(msg)
}

func openErr(err error, o *clickhouse.Options) error {
	proto := "http"
	if o.Protocol == clickhouse.Native {
		proto = "native"
	}
	err = cleanErr(err)
	var pe *plugin.Error
	if errors.As(err, &pe) {
		pe.Message = fmt.Sprintf("can't open a ClickHouse session at %s (%s): %s", o.Addr[0], proto, pe.Message)
		return pe
	}
	hint := ""
	m := strings.ToLower(err.Error())
	switch {
	case strings.Contains(m, "connection refused") || strings.Contains(m, "no such host") ||
		strings.Contains(m, "i/o timeout") || strings.Contains(m, "deadline exceeded"):
		hint = " (check `host`, `port` and `driver`)"
	case strings.Contains(m, "malformed http response") || strings.Contains(m, "unexpected packet"):
		hint = " (the port may be for the other protocol: check `driver` and `port`)"
	case strings.Contains(m, "tls") || strings.Contains(m, "certificate"):
		hint = " (check `secure`, `verify` and `ca_cert`)"
	}
	return &plugin.Error{Kind: "connection", Message: fmt.Sprintf("can't open a ClickHouse session at %s (%s)%s: %v", o.Addr[0], proto, hint, err)}
}
