package main

// The plugin through the protocol loop, against a real ClickHouse server, user `dre`, password
// `dre`. Skipped unless the server is given:
//
//   - DRE_TEST_CLICKHOUSE_HTTP and DRE_TEST_CLICKHOUSE_NATIVE: host:port of its HTTP (8123) and
//     native (9000) ports, e.g. after
//
//	docker run -p 8123:8123 -p 9000:9000 -e CLICKHOUSE_USER=dre -e CLICKHOUSE_PASSWORD=dre clickhouse/clickhouse-server
//
//   - DRE_TEST_CLICKHOUSE_HTTPS, DRE_TEST_CLICKHOUSE_NATIVE_TLS, DRE_TEST_CLICKHOUSE_CA and
//     DRE_TEST_SSH_BASTION, DRE_TEST_SSH_KEY: the TLS server and the bastion from
//     .github/scripts/ssh_bastion.sh.

import (
	"net"
	"os"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/apache/arrow-go/v18/arrow"
	"github.com/apache/arrow-go/v18/arrow/array"
	"golang.org/x/crypto/ssh"

	"github.com/get-dre/dre/go/plugin/plugintest"
)

// server is the connection fields for a test server reached at the address in env.
func server(t *testing.T, env, driver string) map[string]any {
	t.Helper()
	addr := os.Getenv(env)
	if addr == "" {
		t.Skipf("set %s", env)
	}
	host, port, _ := net.SplitHostPort(addr)
	p, _ := strconv.Atoi(port)
	return map[string]any{"driver": driver, "host": host, "port": float64(p), "user": "dre", "password": "dre"}
}

// eachDriver runs f over HTTP and over native.
func eachDriver(t *testing.T, f func(t *testing.T, conn map[string]any)) {
	for _, d := range []struct{ name, env string }{{"http", "DRE_TEST_CLICKHOUSE_HTTP"}, {"native", "DRE_TEST_CLICKHOUSE_NATIVE"}} {
		t.Run(d.name, func(t *testing.T) { f(t, server(t, d.env, d.name)) })
	}
}

func started(t *testing.T, conn map[string]any) *plugintest.Conversation {
	t.Helper()
	c := plugintest.Start(t, pkg, sourceRole)
	c.Hello()
	c.Open(conn)
	return c
}

func cell(t *testing.T, recs []arrow.Record, col string) string {
	t.Helper()
	for _, r := range recs {
		for i, f := range r.Schema().Fields() {
			if f.Name == col && r.NumRows() > 0 {
				return r.Column(i).ValueStr(0)
			}
		}
	}
	t.Fatalf("no column %s", col)
	return ""
}

func TestStatementsShareASession(t *testing.T) {
	eachDriver(t, func(t *testing.T, conn map[string]any) {
		c := started(t, conn)
		c.Exec("CREATE TEMPORARY TABLE recent AS SELECT 1 AS id, 'a' AS name")
		c.Exec("SET max_threads = 3")
		recs := c.Query("SELECT name, getSetting('max_threads') AS threads FROM recent")
		if got := cell(t, recs, "name") + cell(t, recs, "threads"); got != "a3" {
			t.Fatalf("%q", got)
		}
	})
}

func TestAnErrorKeepsTheHTTPSessionAndExplainsTheNativeOne(t *testing.T) {
	eachDriver(t, func(t *testing.T, conn map[string]any) {
		c := started(t, conn)
		c.Exec("CREATE TEMPORARY TABLE kept AS SELECT 1 AS id")
		c.Send(map[string]any{"type": "execute", "sql": "SELECT nope"})
		plugintest.ExpectError(t, c.Reply(), "nope")
		if conn["driver"] == "http" {
			c.Query("SELECT id FROM kept")
			return
		}
		c.Send(map[string]any{"type": "execute", "sql": "SELECT id FROM kept"})
		plugintest.ExpectError(t, c.Reply(), "use `driver: http`")
	})
}

func TestALookupLoadsIntoATempTable(t *testing.T) {
	eachDriver(t, func(t *testing.T, conn map[string]any) {
		c := started(t, conn)
		b := array.NewRecordBuilder(mem, arrow.NewSchema([]arrow.Field{
			{Name: "code", Type: arrow.BinaryTypes.String, Nullable: true},
			{Name: "n", Type: arrow.PrimitiveTypes.Int64, Nullable: true},
			{Name: "on", Type: arrow.FixedWidthTypes.Date32, Nullable: true},
		}, nil))
		b.Field(0).(*array.StringBuilder).AppendValues([]string{"x'y`z", "z"}, nil)
		b.Field(1).(*array.Int64Builder).AppendValues([]int64{1, 2}, nil)
		b.Field(2).(*array.Date32Builder).AppendValues([]arrow.Date32{20000, 0}, []bool{true, false})
		c.Send(map[string]any{"type": "load", "name": "codes"})
		c.SendRecord(b.NewRecord())
		c.Send(map[string]any{"type": "result_set_end"})
		r := c.Reply()
		if r["type"] != "loaded" || r["relation"] != "`dre_lookup_codes`" || r["rows"] != 2.0 {
			t.Fatalf("%v", r)
		}
		recs := c.Query("SELECT code, on FROM dre_lookup_codes WHERE n = 1")
		if got := cell(t, recs, "code") + " " + cell(t, recs, "on"); got != "x'y`z 2024-10-04" {
			t.Fatalf("%q", got)
		}
	})
}

func TestTypesFollowTheRule(t *testing.T) {
	eachDriver(t, func(t *testing.T, conn map[string]any) {
		c := started(t, conn)
		recs := c.Query(`SELECT
			toUInt64(18446744073709551615) AS u64, toInt256('-5') AS i256, toDecimal64(12.345, 3) AS d,
			toDecimal256('1.5', 40) AS wide, toNullable(toInt32(7)) AS ni, toLowCardinality('lc') AS lc,
			toDateTime('2026-01-02 03:04:05', 'Asia/Tokyo') AS dt, toDateTime64('2026-01-02 03:04:05.123456789', 9, 'UTC') AS dt9,
			toDate32('1900-01-01') AS d32, [toDate('2026-01-02')] AS dates, map('k', [1, 2]) AS m,
			CAST((1, 'a,b'), 'Tuple(id UInt8, name String)') AS tup, CAST('{"b":1,"a":{"c":[1,2]}}', 'JSON') AS j,
			toFixedString('ab', 4) AS fs, CAST('e1', 'Enum8(\'e1\' = 1)') AS en, toIPv6('::1') AS ip,
			CAST(toUInt64(7), 'Variant(UInt64, String)') AS v, NULL AS nothing
			SETTINGS enable_json_type = 1, allow_experimental_variant_type = 1`)
		for col, want := range map[string]string{
			"u64": "18446744073709551615", "i256": "-5", "d": "12.345", "wide": "1.5000000000000000000000000000000000000000",
			"ni": "7", "lc": "lc", "dt": "2026-01-01T18:04:05Z", "dt9": "2026-01-02T03:04:05.123456789Z", "d32": "1900-01-01",
			"dates": `["2026-01-02"]`, "m": `{"k":[1,2]}`, "tup": `{"id":1,"name":"a,b"}`, "j": `{"a":{"c":[1,2]},"b":1}`,
			"fs": "ab", "en": "e1", "ip": "::1", "v": "7", "nothing": "(null)",
		} {
			if got := cell(t, recs, col); got != want {
				t.Errorf("%s: %q, want %q", col, got, want)
			}
		}
		types := map[string]arrow.Type{}
		for _, f := range recs[0].Schema().Fields() {
			types[f.Name] = f.Type.ID()
		}
		for col, want := range map[string]arrow.Type{"u64": arrow.DECIMAL128, "d": arrow.DECIMAL128, "ni": arrow.INT64,
			"dt": arrow.TIMESTAMP, "d32": arrow.DATE32, "i256": arrow.STRING, "m": arrow.STRING} {
			if types[col] != want {
				t.Errorf("%s: %s, want %s", col, types[col], want)
			}
		}
	})
}

func TestCheckExplainsWithoutRunning(t *testing.T) {
	eachDriver(t, func(t *testing.T, conn map[string]any) {
		c := started(t, conn)
		c.Send(map[string]any{"type": "check", "sql": "SELECT number FROM numbers(3)"})
		if r := c.Reply(); r["type"] != "ok" {
			t.Fatalf("%v", r)
		}
		c.Send(map[string]any{"type": "check", "sql": "SELECT nope FROM numbers(3)"})
		plugintest.ExpectError(t, c.Reply(), "nope")
		// Not a SELECT: parsed only.
		c.Send(map[string]any{"type": "check", "sql": "SET max_threads = 2"})
		if r := c.Reply(); r["type"] != "ok" {
			t.Fatalf("%v", r)
		}
	})
}

func TestAReadOnlySessionRefusesWrites(t *testing.T) {
	eachDriver(t, func(t *testing.T, conn map[string]any) {
		c := plugintest.Start(t, pkg, sourceRole)
		c.Hello()
		c.OpenReadOnly(conn)
		c.Send(map[string]any{"type": "execute", "sql": "CREATE TABLE default.dre_should_not_exist (x Int8) ENGINE = Memory"})
		r := c.Reply()
		plugintest.ExpectError(t, r, "readonly")
		if r["kind"] != "refused" {
			t.Fatalf("%v", r)
		}
		c.Send(map[string]any{"type": "execute", "sql": "SET readonly = 0"})
		plugintest.ExpectError(t, c.Reply(), "readonly")
		// Temp tables and SET still work.
		c.Exec("CREATE TEMPORARY TABLE scratch AS SELECT 1 AS x")
		c.Exec("SET max_threads = 2")
		c.Query("SELECT x FROM scratch")
	})
}

func TestASandboxedSessionRefusesTheOutsideWorld(t *testing.T) {
	eachDriver(t, func(t *testing.T, conn map[string]any) {
		conn["custom_settings"] = map[string]any{"max_execution_time": 30.0}
		c := plugintest.Start(t, pkg, sourceRole)
		c.Hello()
		c.Send(map[string]any{"type": "open", "connection": conn, "read_only": true, "sandbox": true})
		if r := c.Reply(); r["type"] != "ok" {
			t.Fatalf("%v", r)
		}
		for _, sql := range []string{
			"SELECT * FROM url('http://localhost:8123/ping', 'LineAsString')",
			"SELECT * FROM file('x.csv', 'LineAsString')",
			"CREATE TEMPORARY TABLE scratch (x Int8)",
			"SET readonly = 0",
			"SELECT 1 SETTINGS readonly = 0",
		} {
			c.Send(map[string]any{"type": "execute", "sql": sql})
			plugintest.ExpectError(t, c.Reply(), "readonly")
		}
		// The profile's settings still apply.
		if got := cell(t, c.Query("SELECT toString(getSetting('max_execution_time')) AS s"), "s"); got != "30" {
			t.Fatal(got)
		}
	})
}

func TestCancelStopsTheQueryOnTheServer(t *testing.T) {
	eachDriver(t, func(t *testing.T, conn map[string]any) {
		c := started(t, conn)
		c.Send(map[string]any{"type": "execute", "id": 7,
			"sql": "SELECT sleepEachRow(1) FROM numbers(30) SETTINGS max_block_size = 1, function_sleep_max_microseconds_per_block = 0, log_comment = 'dre-cancel-test'"})
		time.Sleep(1500 * time.Millisecond)
		c.Send(map[string]any{"type": "cancel", "id": 7})
		r := c.Reply()
		if r["type"] == "result" {
			// Native streams the header before the rows; the batch isn't full yet.
			r = c.Reply()
		}
		if r["type"] != "error" || r["kind"] != "cancelled" {
			t.Fatalf("%v", r)
		}
		other := started(t, conn)
		for i := 0; ; i++ {
			recs := other.Query("SELECT count() AS n FROM system.processes WHERE query LIKE '%dre-cancel-test%' AND query NOT LIKE '%system.processes%'")
			if cell(t, recs, "n") == "0" {
				return
			}
			if i == 20 {
				t.Fatal("the query is still running on the server")
			}
			time.Sleep(250 * time.Millisecond)
		}
	})
}

func TestBadCredentialsAreAnAuthError(t *testing.T) {
	eachDriver(t, func(t *testing.T, conn map[string]any) {
		conn["password"] = "wrong"
		c := plugintest.Start(t, pkg, sourceRole)
		c.Hello()
		c.Send(map[string]any{"type": "open", "connection": conn, "read_only": false})
		r := c.Reply()
		plugintest.ExpectError(t, r, "can't open a ClickHouse session")
		if r["kind"] != "auth" {
			t.Fatalf("%v", r)
		}
	})
}

func TestTLSWithAPrivateCA(t *testing.T) {
	ca := os.Getenv("DRE_TEST_CLICKHOUSE_CA")
	for _, d := range []struct{ name, env string }{{"http", "DRE_TEST_CLICKHOUSE_HTTPS"}, {"native", "DRE_TEST_CLICKHOUSE_NATIVE_TLS"}} {
		t.Run(d.name, func(t *testing.T) {
			conn := server(t, d.env, d.name)
			conn["secure"], conn["ca_cert"], conn["server_host_name"] = true, ca, "ch.internal"
			c := started(t, conn)
			if got := cell(t, c.Query("SELECT 1 AS one"), "one"); got != "1" {
				t.Fatal(got)
			}
			// Without the CA the certificate isn't trusted.
			delete(conn, "ca_cert")
			c2 := plugintest.Start(t, pkg, sourceRole)
			c2.Hello()
			c2.Send(map[string]any{"type": "open", "connection": conn, "read_only": false})
			plugintest.ExpectError(t, c2.Reply(), "certificate")
		})
	}
}

func TestThroughAnSSHBastion(t *testing.T) {
	bastion, key, ca := os.Getenv("DRE_TEST_SSH_BASTION"), os.Getenv("DRE_TEST_SSH_KEY"), os.Getenv("DRE_TEST_CLICKHOUSE_CA")
	if bastion == "" || key == "" {
		t.Skip("run .github/scripts/ssh_bastion.sh and set DRE_TEST_SSH_BASTION, DRE_TEST_SSH_KEY")
	}
	host, port, _ := net.SplitHostPort(bastion)
	p, _ := strconv.Atoi(port)
	known := t.TempDir() + "/known_hosts"
	for _, d := range []struct {
		name string
		port float64
	}{{"http", 8443}, {"native", 9440}} {
		t.Run(d.name, func(t *testing.T) {
			block := map[string]any{"host": host, "port": float64(p), "username": "dre", "private_key_path": key, "known_hosts_path": known}
			conn := map[string]any{"driver": d.name, "host": "ch.internal", "port": d.port, "user": "dre", "password": "dre",
				"secure": true, "ca_cert": ca, "ssh": block}
			// An unknown host key is refused, with how to trust it.
			c := plugintest.Start(t, pkg, sourceRole)
			c.Hello()
			c.Send(map[string]any{"type": "open", "connection": conn, "read_only": false})
			plugintest.ExpectError(t, c.Reply(), "isn't in known_hosts; its host key is "+bastionFingerprint(t, host, p))
			// A wrong pinned fingerprint is refused; the right one works.
			block["host_key_fingerprint"] = "SHA256:nope"
			c.Send(map[string]any{"type": "open", "connection": conn, "read_only": false})
			plugintest.ExpectError(t, c.Reply(), "not the pinned")
			block["host_key_fingerprint"] = bastionFingerprint(t, host, p)
			c.Open(conn)
			if got := cell(t, c.Query("SELECT hostName() = 'ch.internal' OR hostName() != '' AS ok"), "ok"); got != "1" {
				t.Fatal(got)
			}
		})
	}
}

// bastionFingerprint asks the bastion for its host key, as `ssh-keyscan | ssh-keygen -lf` would.
func bastionFingerprint(t *testing.T, host string, port int) string {
	t.Helper()
	var fp string
	conf := &ssh.ClientConfig{User: "dre", Timeout: 10 * time.Second,
		HostKeyCallback: func(_ string, _ net.Addr, key ssh.PublicKey) error { fp = ssh.FingerprintSHA256(key); return nil }}
	if c, err := ssh.Dial("tcp", net.JoinHostPort(host, strconv.Itoa(port)), conf); err == nil {
		c.Close()
	}
	if !strings.HasPrefix(fp, "SHA256:") {
		t.Fatalf("no host key from the bastion: %q", fp)
	}
	return fp
}
