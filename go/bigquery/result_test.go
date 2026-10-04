package main

import (
	"encoding/json"
	"io"
	"math/big"
	"strings"
	"testing"
	"time"

	"cloud.google.com/go/bigquery"
	"cloud.google.com/go/civil"
	"github.com/apache/arrow-go/v18/arrow"
	"github.com/apache/arrow-go/v18/arrow/array"
	"github.com/apache/arrow-go/v18/arrow/decimal128"
	"github.com/apache/arrow-go/v18/arrow/decimal256"
	"google.golang.org/api/iterator"

	"github.com/get-dre/dre/go/plugin"
)

// One row of every BigQuery type, as the REST API returns it and as the Storage Read API does.
var allTypes = bigquery.Schema{
	{Name: "i", Type: bigquery.IntegerFieldType},
	{Name: "f", Type: bigquery.FloatFieldType},
	{Name: "b", Type: bigquery.BooleanFieldType},
	{Name: "s", Type: bigquery.StringFieldType},
	{Name: "n", Type: bigquery.NumericFieldType},
	{Name: "bn", Type: bigquery.BigNumericFieldType},
	{Name: "d", Type: bigquery.DateFieldType},
	{Name: "t", Type: bigquery.TimeFieldType},
	{Name: "dt", Type: bigquery.DateTimeFieldType},
	{Name: "ts", Type: bigquery.TimestampFieldType},
	{Name: "g", Type: bigquery.GeographyFieldType},
	{Name: "j", Type: bigquery.JSONFieldType},
	{Name: "iv", Type: bigquery.IntervalFieldType},
	{Name: "rec", Type: bigquery.RecordFieldType, Schema: bigquery.Schema{
		{Name: "id", Type: bigquery.IntegerFieldType},
		{Name: "tags", Type: bigquery.StringFieldType, Repeated: true},
	}},
	{Name: "arr", Type: bigquery.IntegerFieldType, Repeated: true},
	{Name: "rng", Type: bigquery.RangeFieldType, RangeElementType: &bigquery.RangeElementType{Type: bigquery.DateFieldType}},
}

var when = time.Date(2026, 1, 2, 3, 4, 5, 123456000, time.UTC)

func restRow() []bigquery.Value {
	bn, _ := new(big.Rat).SetString("123456789012345678901234567890.5")
	iv, _ := bigquery.ParseInterval("1-2 3 4:5:6.5")
	return []bigquery.Value{
		int64(42), 1.5, true, "a,b \"q\"\nline2", big.NewRat(1234567, 100), bn,
		civil.DateOf(when), civil.TimeOf(when), civil.DateTimeOf(when), when,
		"POINT(1 2)", "{\n  \"b\": 1,\n  \"a\": [1, 2]\n}", iv,
		[]bigquery.Value{int64(7), []bigquery.Value{"x", "y"}},
		[]bigquery.Value{int64(1), int64(2)},
		&bigquery.RangeValue{Start: civil.DateOf(when), End: nil},
	}
}

// storageRecord is the same row as the Storage Read API sends it: nested Arrow, JSON
// pretty-printed, the interval as BigQuery's canonical text.
func storageRecord(t *testing.T) arrow.Record {
	fields := arrowSchema(allTypes).Fields()
	fields[12] = arrow.Field{Name: "iv", Type: arrow.BinaryTypes.String, Nullable: true}
	b := array.NewRecordBuilder(mem, arrow.NewSchema(fields, nil))
	defer b.Release()
	b.Field(0).(*array.Int64Builder).Append(42)
	b.Field(1).(*array.Float64Builder).Append(1.5)
	b.Field(2).(*array.BooleanBuilder).Append(true)
	b.Field(3).(*array.StringBuilder).Append("a,b \"q\"\nline2")
	n, _ := decimal128.FromString("12345.67", 38, 9)
	b.Field(4).(*array.Decimal128Builder).Append(n)
	bn, _ := decimal256.FromString("123456789012345678901234567890.5", 76, 38)
	b.Field(5).(*array.Decimal256Builder).Append(bn)
	b.Field(6).(*array.Date32Builder).Append(arrow.Date32FromTime(when))
	b.Field(7).(*array.Time64Builder).Append(arrow.Time64((3*3600+4*60+5)*1e6 + 123456))
	b.Field(8).(*array.TimestampBuilder).Append(arrow.Timestamp(when.UnixMicro()))
	b.Field(9).(*array.TimestampBuilder).Append(arrow.Timestamp(when.UnixMicro()))
	b.Field(10).(*array.StringBuilder).Append("POINT(1 2)")
	b.Field(11).(*array.StringBuilder).Append("{\n  \"b\": 1,\n  \"a\": [1, 2]\n}")
	b.Field(12).(*array.StringBuilder).Append("1-2 3 4:5:6.5")
	rec := b.Field(13).(*array.StructBuilder)
	rec.Append(true)
	rec.FieldBuilder(0).(*array.Int64Builder).Append(7)
	tags := rec.FieldBuilder(1).(*array.ListBuilder)
	tags.Append(true)
	tags.ValueBuilder().(*array.StringBuilder).AppendValues([]string{"x", "y"}, nil)
	arr := b.Field(14).(*array.ListBuilder)
	arr.Append(true)
	arr.ValueBuilder().(*array.Int64Builder).AppendValues([]int64{1, 2}, nil)
	rng := b.Field(15).(*array.StructBuilder)
	rng.Append(true)
	rng.FieldBuilder(0).(*array.Date32Builder).Append(arrow.Date32FromTime(when))
	rng.FieldBuilder(1).(*array.Date32Builder).AppendNull()
	return b.NewRecord()
}

// sent is what core receives for rec: the shared type rule applied, each cell as text.
func sent(t *testing.T, rec arrow.Record) ([]string, *arrow.Schema) {
	t.Helper()
	r := &result{bq: allTypes, schema: arrowSchema(allTypes)}
	norm, err := r.normalise(rec)
	if err != nil {
		t.Fatal(err)
	}
	schema := plugin.OutputSchema(r.schema)
	out, err := plugin.Convert(norm, schema)
	if err != nil {
		t.Fatal(err)
	}
	cells := make([]string, out.NumCols())
	for i := range cells {
		cells[i] = out.Column(i).ValueStr(0)
	}
	return cells, out.Schema()
}

func TestBothAPIsGiveTheSameOutput(t *testing.T) {
	rows := [][]bigquery.Value{restRow()}
	next := func(row *[]bigquery.Value) error {
		if len(rows) == 0 {
			return iterator.Done
		}
		*row, rows = rows[0], rows[1:]
		return nil
	}
	rest, err := restBatch(next, allTypes, arrowSchema(allTypes), 10)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := restBatch(next, allTypes, arrowSchema(allTypes), 10); err != io.EOF {
		t.Fatalf("after the last row: %v", err)
	}
	fromREST, schema := sent(t, rest)
	fromStorage, schema2 := sent(t, storageRecord(t))
	if !schema.Equal(schema2) {
		t.Fatalf("schemas differ:\n%s\n%s", schema, schema2)
	}
	for i := range fromREST {
		if fromREST[i] != fromStorage[i] {
			t.Errorf("column `%s`: REST %q, Storage %q", allTypes[i].Name, fromREST[i], fromStorage[i])
		}
	}
	want := map[string]string{
		"s":   "a,b \"q\"\nline2",
		"bn":  "123456789012345678901234567890.5",
		"j":   `{"b":1,"a":[1,2]}`,
		"iv":  "P1Y2M3DT4H5M6.5S",
		"rec": `{"id":7,"tags":["x","y"]}`,
		"arr": `[1,2]`,
		"rng": `{"start":"2026-01-02","end":null}`,
	}
	for i, f := range allTypes {
		if w, ok := want[f.Name]; ok && fromREST[i] != w {
			t.Errorf("column `%s`: %q, want %q", f.Name, fromREST[i], w)
		}
	}
	// Scalars keep their types; zones are kept apart.
	types := map[string]arrow.Type{"i": arrow.INT64, "n": arrow.DECIMAL128, "d": arrow.DATE32, "t": arrow.TIME64, "bn": arrow.STRING, "rec": arrow.STRING}
	for i, f := range schema.Fields() {
		if w, ok := types[f.Name]; ok && f.Type.ID() != w {
			t.Errorf("column `%s` is %s", f.Name, f.Type)
		}
		if f.Name == "ts" && f.Type.(*arrow.TimestampType).TimeZone == "" || f.Name == "dt" && f.Type.(*arrow.TimestampType).TimeZone != "" {
			t.Errorf("column `%s` zone: %s", f.Name, f.Type)
		}
		_ = i
	}
	for _, c := range fromREST {
		if strings.HasPrefix(c, "{") && !json.Valid([]byte(c)) {
			t.Errorf("not JSON: %s", c)
		}
	}
}

func TestTempTableSQLTypesEveryColumnAndEscapes(t *testing.T) {
	s := arrow.NewSchema([]arrow.Field{
		{Name: "code", Type: arrow.BinaryTypes.String, Nullable: true},
		{Name: "n", Type: arrow.PrimitiveTypes.Int64, Nullable: true},
		{Name: "d", Type: arrow.FixedWidthTypes.Date32, Nullable: true},
	}, nil)
	b := array.NewRecordBuilder(mem, s)
	b.Field(0).(*array.StringBuilder).AppendValues([]string{`it's "x"` + "\n\\", ""}, []bool{true, false})
	b.Field(1).(*array.Int64Builder).AppendValues([]int64{1, 2}, nil)
	b.Field(2).(*array.Date32Builder).AppendValues([]arrow.Date32{arrow.Date32FromTime(when), 0}, []bool{true, false})
	rec := b.NewRecord()
	sql, n, err := tempTableSQL("dre_lookup_codes", s, []arrow.Record{rec})
	if err != nil || n != 2 {
		t.Fatal(err, n)
	}
	want := "CREATE OR REPLACE TEMP TABLE `dre_lookup_codes` AS SELECT * FROM UNNEST(ARRAY<STRUCT<`code` STRING, `n` INT64, `d` DATE>>[\n" +
		"  (\"it's \\\"x\\\"\\n\\\\\", 1, DATE '2026-01-02'),\n  (NULL, 2, NULL)\n])"
	if sql != want {
		t.Fatalf("%s", sql)
	}
	empty, n, _ := tempTableSQL("dre_lookup_none", s, nil)
	if n != 0 || !strings.HasSuffix(empty, ">>[\n  \n])") {
		t.Fatalf("%s", empty)
	}
}
