package main

// The plugin through the protocol loop against a real Snowflake account. Skipped unless
// DRE_TEST_SNOWFLAKE_ACCOUNT and DRE_TEST_SNOWFLAKE_USER are set, with
// DRE_TEST_SNOWFLAKE_PRIVATE_KEY_PATH (key-pair) or DRE_TEST_SNOWFLAKE_PASSWORD, and optionally
// DRE_TEST_SNOWFLAKE_WAREHOUSE, _DATABASE, _SCHEMA and _ROLE. A trial account works.

import (
	"os"
	"strings"
	"testing"

	"github.com/apache/arrow-go/v18/arrow"
	"github.com/apache/arrow-go/v18/arrow/array"

	"github.com/get-dre/dre/go/plugin/plugintest"
)

func live(t *testing.T) *plugintest.Conversation {
	t.Helper()
	env := func(k string) string { return os.Getenv("DRE_TEST_SNOWFLAKE_" + k) }
	if env("ACCOUNT") == "" || env("USER") == "" {
		t.Skip("set DRE_TEST_SNOWFLAKE_ACCOUNT, _USER and _PRIVATE_KEY_PATH or _PASSWORD")
	}
	conn := map[string]any{"account": env("ACCOUNT"), "user": env("USER"), "query_tag": "dre-tests"}
	for _, k := range []string{"PRIVATE_KEY_PATH", "PRIVATE_KEY_PASSPHRASE", "PASSWORD", "WAREHOUSE", "DATABASE", "SCHEMA", "ROLE", "AUTHENTICATOR", "TOKEN"} {
		if v := env(k); v != "" {
			conn[strings.ToLower(k)] = v
		}
	}
	c := plugintest.Start(t, pkg, sourceRole)
	c.Hello()
	c.Open(conn)
	return c
}

func value(t *testing.T, recs []arrow.Record, col string) string {
	t.Helper()
	for _, r := range recs {
		for i, f := range r.Schema().Fields() {
			if strings.EqualFold(f.Name, col) && r.NumRows() > 0 {
				return r.Column(i).ValueStr(0)
			}
		}
	}
	t.Fatalf("no column %s", col)
	return ""
}

func TestLiveStatementsShareASessionAndLoadWorks(t *testing.T) {
	c := live(t)
	c.Exec("CREATE TEMPORARY TABLE recent AS SELECT 1 AS id, 'a' AS name")
	if got := value(t, c.Query("SELECT name FROM recent"), "NAME"); got != "a" {
		t.Fatalf("%q", got)
	}
	b := array.NewRecordBuilder(mem, arrow.NewSchema([]arrow.Field{{Name: "code", Type: arrow.BinaryTypes.String, Nullable: true}}, nil))
	b.Field(0).(*array.StringBuilder).AppendValues([]string{`x'y\z`}, nil)
	c.Send(map[string]any{"type": "load", "name": "codes"})
	c.SendRecord(b.NewRecord())
	c.Send(map[string]any{"type": "result_set_end"})
	if r := c.Reply(); r["type"] != "loaded" || r["rows"] != 1.0 {
		t.Fatalf("%v", r)
	}
	if got := value(t, c.Query(`SELECT "code" FROM "dre_lookup_codes"`), "code"); got != `x'y\z` {
		t.Fatalf("%q", got)
	}
}

func TestLiveTypesFollowTheRule(t *testing.T) {
	c := live(t)
	recs := c.Query(`SELECT
		PARSE_JSON('{"b": 1, "a": [1, 2], "s": "x,y"}') AS v,
		ARRAY_CONSTRUCT(1, 2, 3) AS arr,
		12.34::NUMBER(10,2) AS amount,
		SEQ8() AS n,
		'2026-01-02 03:04:05 +02:00'::TIMESTAMP_TZ AS tz,
		'2026-01-02 03:04:05'::TIMESTAMP_NTZ AS ntz`)
	if v := value(t, recs, "V"); v != `{"a":[1,2],"b":1,"s":"x,y"}` && v != `{"b":1,"a":[1,2],"s":"x,y"}` {
		t.Errorf("variant: %q", v)
	}
	if a := value(t, recs, "ARR"); a != "[1,2,3]" {
		t.Errorf("array: %q", a)
	}
	if a := value(t, recs, "AMOUNT"); a != "12.34" {
		t.Errorf("amount: %q", a)
	}
	for _, f := range recs[0].Schema().Fields() {
		if f.Name == "TZ" && f.Type.(*arrow.TimestampType).TimeZone == "" || f.Name == "NTZ" && f.Type.(*arrow.TimestampType).TimeZone != "" {
			t.Errorf("%s: %s", f.Name, f.Type)
		}
	}
}

func TestLiveCheckExplains(t *testing.T) {
	c := live(t)
	c.Send(map[string]any{"type": "check", "sql": "SELECT 1"})
	if r := c.Reply(); r["type"] != "ok" {
		t.Fatalf("%v", r)
	}
	c.Send(map[string]any{"type": "check", "sql": "SELECT nope FROM no_such_table"})
	plugintest.ExpectError(t, c.Reply(), "no_such_table")
}
