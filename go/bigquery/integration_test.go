package main

// The plugin through the protocol loop, against BigQuery itself or goccy/bigquery-emulator.
// Skipped unless one is given:
//
//   - DRE_TEST_BIGQUERY_PROJECT: a real project, signed in with gcloud's application-default
//     credentials (`gcloud auth application-default login`); DRE_TEST_BIGQUERY_LOCATION
//     optionally. A free BigQuery sandbox project works.
//   - DRE_TEST_BIGQUERY_EMULATOR: the emulator's REST address, e.g. http://localhost:9050, with a
//     project named dre-test:
//
//	docker run -p 9050:9050 ghcr.io/goccy/bigquery-emulator --project=dre-test --dataset=shop
//
// The emulator has no sessions and its dry runs don't analyse SQL, so those tests run live only.

import (
	"os"
	"strings"
	"testing"

	"github.com/apache/arrow-go/v18/arrow"
	"github.com/apache/arrow-go/v18/arrow/array"

	"github.com/get-dre/dre/go/plugin/plugintest"
)

// target opens a session on the BigQuery under test; live says it's the real service.
func target(t *testing.T) (c *plugintest.Conversation, live bool) {
	t.Helper()
	c = plugintest.Start(t, pkg, sourceRole)
	if p := os.Getenv("DRE_TEST_BIGQUERY_PROJECT"); p != "" {
		c.Hello()
		c.Open(map[string]any{"method": "oauth", "project": p, "location": os.Getenv("DRE_TEST_BIGQUERY_LOCATION")})
		return c, true
	}
	url := os.Getenv("DRE_TEST_BIGQUERY_EMULATOR")
	if url == "" {
		t.Skip("set DRE_TEST_BIGQUERY_PROJECT or DRE_TEST_BIGQUERY_EMULATOR")
	}
	c.Hello()
	// The emulator takes any token.
	c.Open(map[string]any{"method": "oauth-secrets", "token": "emulator", "project": "dre-test", "dataset": "shop", "api_endpoint": url})
	return c, false
}

func liveOnly(t *testing.T) *plugintest.Conversation {
	t.Helper()
	c, live := target(t)
	if !live {
		t.Skip("the emulator doesn't do this; set DRE_TEST_BIGQUERY_PROJECT")
	}
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
	c := liveOnly(t)
	c.Exec("CREATE TEMP TABLE recent AS SELECT 1 AS id, 'a' AS name")
	recs := c.Query("SELECT name FROM recent")
	if got := cell(t, recs, "name"); got != "a" {
		t.Fatalf("%q", got)
	}
}

func TestALookupLoadsIntoATempTable(t *testing.T) {
	c := liveOnly(t)
	b := array.NewRecordBuilder(mem, arrow.NewSchema([]arrow.Field{
		{Name: "code", Type: arrow.BinaryTypes.String, Nullable: true},
		{Name: "n", Type: arrow.PrimitiveTypes.Int64, Nullable: true},
	}, nil))
	b.Field(0).(*array.StringBuilder).AppendValues([]string{"x'y", "z"}, nil)
	b.Field(1).(*array.Int64Builder).AppendValues([]int64{1, 2}, nil)
	c.Send(map[string]any{"type": "load", "name": "codes"})
	c.SendRecord(b.NewRecord())
	c.Send(map[string]any{"type": "result_set_end"})
	r := c.Reply()
	if r["type"] != "loaded" || r["relation"] != "dre_lookup_codes" || r["rows"] != 2.0 {
		t.Fatalf("%v", r)
	}
	recs := c.Query("SELECT code FROM dre_lookup_codes WHERE n = 1")
	if got := cell(t, recs, "code"); got != "x'y" {
		t.Fatalf("%q", got)
	}
}

func TestTypesFollowTheRule(t *testing.T) {
	c, _ := target(t)
	recs := c.Query(`SELECT
		STRUCT(1 AS id, 'a,b' AS name, ['x', 'y'] AS tags) AS s,
		[1, 2, 3] AS arr,
		JSON '{"b": 1, "a": [1, 2]}' AS j,
		NUMERIC '12345.67' AS n,
		TIMESTAMP '2026-01-02 03:04:05 UTC' AS ts,
		DATETIME '2026-01-02 03:04:05' AS dt,
		CAST(NULL AS STRING) AS nothing`)
	for col, want := range map[string]string{
		"s": `{"id":1,"name":"a,b","tags":["x","y"]}`, "arr": "[1,2,3]",
	} {
		if got := cell(t, recs, col); got != want {
			t.Errorf("%s: %q, want %q", col, got, want)
		}
	}
	if j := cell(t, recs, "j"); strings.ContainsAny(j, "\n ") {
		t.Errorf("json not compact: %q", j)
	}
	schema := recs[0].Schema()
	for _, f := range schema.Fields() {
		switch f.Name {
		case "n":
			if f.Type.ID() != arrow.DECIMAL128 {
				t.Errorf("n: %s", f.Type)
			}
		case "ts":
			if f.Type.(*arrow.TimestampType).TimeZone == "" {
				t.Errorf("ts: %s", f.Type)
			}
		case "dt":
			if f.Type.(*arrow.TimestampType).TimeZone != "" {
				t.Errorf("dt: %s", f.Type)
			}
		}
	}
}

func TestCheckDryRunsTheStatement(t *testing.T) {
	c, live := target(t)
	c.Send(map[string]any{"type": "check", "sql": "SELECT 1"})
	if r := c.Reply(); r["type"] != "ok" {
		t.Fatalf("%v", r)
	}
	if !live {
		return // the emulator's dry run doesn't analyse the statement
	}
	c.Send(map[string]any{"type": "check", "sql": "SELECT nope FROM UNNEST([1]) AS x"})
	plugintest.ExpectError(t, c.Reply(), "nope")
}

func TestMaximumBytesBilledStopsAnExpensiveQuery(t *testing.T) {
	p := os.Getenv("DRE_TEST_BIGQUERY_PROJECT")
	if p == "" {
		t.Skip("set DRE_TEST_BIGQUERY_PROJECT")
	}
	c := plugintest.Start(t, pkg, sourceRole)
	c.Hello()
	c.Open(map[string]any{"method": "oauth", "project": p, "location": os.Getenv("DRE_TEST_BIGQUERY_LOCATION"), "maximum_bytes_billed": 1.0})
	// A public table scan bills at least 10 MB.
	c.Send(map[string]any{"type": "execute", "sql": "SELECT COUNT(DISTINCT word) FROM `bigquery-public-data.samples.shakespeare`"})
	plugintest.ExpectError(t, c.Reply(), "bytes billed")
}
