package main

import (
	"errors"
	"fmt"
	"net"
	"strings"
	"testing"
	"time"

	"github.com/apache/arrow-go/v18/arrow"
	"github.com/apache/arrow-go/v18/arrow/array"
	"github.com/apache/arrow-go/v18/arrow/memory"

	"github.com/get-dre/dre/go/plugin/plugintest"
)

func TestTempViewSQLEscapesForSparkAndCastsEveryColumn(t *testing.T) {
	schema := arrow.NewSchema([]arrow.Field{
		{Name: "code", Type: arrow.BinaryTypes.String, Nullable: true},
		{Name: "n", Type: arrow.PrimitiveTypes.Int64, Nullable: true},
		{Name: "d", Type: arrow.FixedWidthTypes.Date32, Nullable: true},
	}, nil)
	b := array.NewRecordBuilder(memory.DefaultAllocator, schema)
	defer b.Release()
	b.Field(0).(*array.StringBuilder).AppendValues([]string{`O'Brien \ Co`, ""}, []bool{true, false})
	b.Field(1).(*array.Int64Builder).AppendValues([]int64{1, 0}, []bool{true, false})
	b.Field(2).(*array.Date32Builder).AppendValues([]arrow.Date32{20454, 0}, []bool{true, false})
	rec := b.NewRecord()
	defer rec.Release()
	sql, n, err := tempViewSQL("dre_lookup_x", schema, []arrow.Record{rec})
	want := "CREATE OR REPLACE TEMPORARY VIEW dre_lookup_x AS SELECT CAST(`code` AS STRING) AS `code`, " +
		"CAST(`n` AS BIGINT) AS `n`, CAST(`d` AS DATE) AS `d` FROM VALUES\n  " +
		`('O\'Brien \\ Co', 1, DATE'2026-01-01'),` + "\n  (NULL, NULL, NULL)\nAS t(`code`, `n`, `d`)"
	if err != nil || n != 2 || sql != want {
		t.Fatalf("%v %d\n%s\nwant\n%s", err, n, sql, want)
	}
	sql, _, _ = tempViewSQL("v", schema, nil)
	if !strings.HasSuffix(sql, "VALUES (NULL, NULL, NULL) AS t(`code`, `n`, `d`) WHERE 1 = 0") {
		t.Fatal(sql)
	}
	bad := arrow.NewSchema([]arrow.Field{{Name: "b", Type: arrow.BinaryTypes.Binary}}, nil)
	if _, _, err := tempViewSQL("v", bad, nil); err == nil {
		t.Fatal("binary accepted")
	}
}

func TestErrorsKeepTheMessageNotTheStackTrace(t *testing.T) {
	err := cleanErr(errors.New("[TABLE_OR_VIEW_NOT_FOUND] The table `x` cannot be found.\n\tat org.apache.spark.Foo(Foo.scala:1)\n\tat more"))
	if err.Error() != "[TABLE_OR_VIEW_NOT_FOUND] The table `x` cannot be found." {
		t.Fatal(err)
	}
	if planError("== Physical Plan ==\n*(1) Project") != "" {
		t.Fatal("a good plan flagged")
	}
}

func TestHostsAndFields(t *testing.T) {
	for in, want := range map[string]string{
		"dbc-1.cloud.databricks.com":          "dbc-1.cloud.databricks.com",
		"https://dbc-1.cloud.databricks.com/": "dbc-1.cloud.databricks.com",
	} {
		if hostname(in) != want {
			t.Fatalf("%s → %s", in, hostname(in))
		}
	}
	if baseURL("dbc-1.cloud.databricks.com/") != "https://dbc-1.cloud.databricks.com" {
		t.Fatal(baseURL("dbc-1.cloud.databricks.com/"))
	}
	if _, err := required(map[string]any{}, "host"); err == nil || err.Error() != "the profile output needs a `host` field" {
		t.Fatal(err)
	}
	if n, ok := number("900"); !ok || n != 900 {
		t.Fatal(n)
	}
}

func TestAnUnreachableWorkspaceFailsAtOnce(t *testing.T) {
	t.Setenv("HTTPS_PROXY", "")
	t.Setenv("https_proxy", "")
	for _, authType := range []string{"oauth", "pat"} {
		start := time.Now()
		_, err := newDatabricks(map[string]any{
			"host": "dbc-does-not-exist.invalid", "http_path": "/sql/1.0/warehouses/x",
			"auth_type": authType, "token": "t",
		})
		if err == nil || !strings.Contains(err.Error(), "doesn't resolve") {
			t.Fatalf("%s: %v", authType, err)
		}
		if time.Since(start) > 10*time.Second {
			t.Fatalf("%s: took %v", authType, time.Since(start))
		}
	}
	// Nothing listens: refused at once.
	l, _ := net.Listen("tcp", "127.0.0.1:0")
	addr := l.Addr().String()
	l.Close()
	err := reachable("https://" + addr)
	if err == nil || !strings.Contains(err.Error(), "can't reach") {
		t.Fatal(err)
	}
}

func TestTheSourceDescribesItsFieldsAndQuote(t *testing.T) {
	c := plugintest.Start(t, pkg, sourceRole)
	h := c.Hello()
	if h["name"] != "databricks" || fmt.Sprint(h["provides"]) != "[source/databricks destination/databricks]" {
		t.Fatalf("%v", h)
	}
	c.Send(map[string]any{"type": "describe"})
	d := c.Reply()
	if plugintest.FieldNames(d) != "host,http_path,auth_type,token,client_id,client_secret,catalog,schema" || d["identifier_quote"] != "`" {
		t.Fatalf("%v", d)
	}
}

func TestAPlanningErrorIsReportedWithoutThePlanTree(t *testing.T) {
	plan := "Error occurred during query planning: \n[UNRESOLVED_COLUMN.WITHOUT_SUGGESTION] A column `nope` cannot be resolved.\n'Project ['nope]\n+- OneRowRelation"
	e := planError(plan)
	if !strings.Contains(e, "UNRESOLVED_COLUMN") || strings.Contains(e, "Project") {
		t.Fatal(e)
	}
	if planError("== Physical Plan ==\n*(1) Project [1 AS 1#0]") != "" {
		t.Fatal("a good plan was an error")
	}
}
