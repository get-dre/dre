package plugin

import (
	"strings"
	"testing"
	"time"

	"github.com/apache/arrow-go/v18/arrow"
	"github.com/apache/arrow-go/v18/arrow/array"
	"github.com/apache/arrow-go/v18/arrow/decimal256"
	"github.com/apache/arrow-go/v18/arrow/memory"
)

var mem = memory.DefaultAllocator

func record(t *testing.T, fields []arrow.Field, build func(b *array.RecordBuilder)) arrow.Record {
	t.Helper()
	b := array.NewRecordBuilder(mem, arrow.NewSchema(fields, nil))
	defer b.Release()
	build(b)
	return b.NewRecord()
}

func convert(t *testing.T, rec arrow.Record) arrow.Record {
	t.Helper()
	out, err := Convert(rec, OutputSchema(rec.Schema()))
	if err != nil {
		t.Fatal(err)
	}
	return out
}

func strs(t *testing.T, c arrow.Array) []string {
	t.Helper()
	s, ok := c.(*array.String)
	if !ok {
		t.Fatalf("not text: %s", c.DataType())
	}
	out := make([]string, s.Len())
	for i := range out {
		if s.IsNull(i) {
			out[i] = "<null>"
		} else {
			out[i] = s.Value(i)
		}
	}
	return out
}

func TestNestedValuesBecomeCompactSingleLineJSON(t *testing.T) {
	tsType := &arrow.TimestampType{Unit: arrow.Microsecond, TimeZone: "UTC"}
	structType := arrow.StructOf(
		arrow.Field{Name: "id", Type: arrow.PrimitiveTypes.Int32},
		arrow.Field{Name: "name", Type: arrow.BinaryTypes.String},
		arrow.Field{Name: "tags", Type: arrow.ListOf(arrow.BinaryTypes.String)},
		arrow.Field{Name: "ts", Type: tsType},
	)
	mapType := arrow.MapOf(arrow.BinaryTypes.String, arrow.PrimitiveTypes.Int32)
	rec := record(t, []arrow.Field{
		{Name: "s", Type: structType, Nullable: true},
		{Name: "m", Type: mapType, Nullable: true},
		{Name: "l", Type: arrow.ListOf(arrow.PrimitiveTypes.Float64), Nullable: true},
	}, func(b *array.RecordBuilder) {
		sb := b.Field(0).(*array.StructBuilder)
		sb.Append(true)
		sb.FieldBuilder(0).(*array.Int32Builder).Append(1)
		sb.FieldBuilder(1).(*array.StringBuilder).Append("a,b \"q\"\nline2")
		lb := sb.FieldBuilder(2).(*array.ListBuilder)
		lb.Append(true)
		lb.ValueBuilder().(*array.StringBuilder).AppendValues([]string{"x", "y"}, nil)
		ts, _ := arrow.TimestampFromTime(time.Date(2026, 1, 2, 3, 4, 5, 0, time.UTC), arrow.Microsecond)
		sb.FieldBuilder(3).(*array.TimestampBuilder).Append(ts)
		sb.AppendNull()
		sb.FieldBuilder(0).(*array.Int32Builder).AppendNull()
		sb.FieldBuilder(1).(*array.StringBuilder).AppendNull()
		sb.FieldBuilder(2).(*array.ListBuilder).AppendNull()
		sb.FieldBuilder(3).(*array.TimestampBuilder).AppendNull()

		mb := b.Field(1).(*array.MapBuilder)
		mb.Append(true)
		mb.KeyBuilder().(*array.StringBuilder).AppendValues([]string{"k1", "k2"}, nil)
		mb.ItemBuilder().(*array.Int32Builder).AppendValues([]int32{1, 2}, nil)
		mb.Append(true)

		fl := b.Field(2).(*array.ListBuilder)
		fl.Append(true)
		fl.ValueBuilder().(*array.Float64Builder).AppendValues([]float64{1.5, 2}, nil)
		fl.Append(true)
		fl.ValueBuilder().(*array.Float64Builder).AppendNull()
	})
	out := convert(t, rec)
	for i := 0; i < 3; i++ {
		if out.Schema().Field(i).Type.ID() != arrow.STRING {
			t.Fatalf("column %d is %s", i, out.Schema().Field(i).Type)
		}
	}
	s := strs(t, out.Column(0))
	if s[0] != `{"id":1,"name":"a,b \"q\"\nline2","tags":["x","y"],"ts":"2026-01-02T03:04:05Z"}` || s[1] != "<null>" {
		t.Fatalf("%q", s)
	}
	if strings.Contains(s[0], "\n") {
		t.Fatal("JSON spans lines")
	}
	if m := strs(t, out.Column(1)); m[0] != `{"k1":1,"k2":2}` || m[1] != `{}` {
		t.Fatalf("%q", m)
	}
	if l := strs(t, out.Column(2)); l[0] != `[1.5,2]` || l[1] != `[null]` {
		t.Fatalf("%q", l)
	}
}

func TestIntegerBatchesOfDifferentSizesGiveOneColumnType(t *testing.T) {
	declared := arrow.NewSchema([]arrow.Field{{Name: "n", Type: arrow.PrimitiveTypes.Int64}}, nil)
	schema := OutputSchema(declared)
	small := record(t, []arrow.Field{{Name: "n", Type: arrow.PrimitiveTypes.Int8}}, func(b *array.RecordBuilder) {
		b.Field(0).(*array.Int8Builder).AppendValues([]int8{1, -2}, nil)
	})
	big := record(t, []arrow.Field{{Name: "n", Type: arrow.PrimitiveTypes.Int64}}, func(b *array.RecordBuilder) {
		b.Field(0).(*array.Int64Builder).Append(1 << 40)
	})
	for _, rec := range []arrow.Record{small, big} {
		out, err := Convert(rec, schema)
		if err != nil {
			t.Fatal(err)
		}
		if !arrow.TypeEqual(out.Schema().Field(0).Type, arrow.PrimitiveTypes.Int64) {
			t.Fatalf("%s", out.Schema())
		}
	}
	out, _ := Convert(small, schema)
	if out.Column(0).(*array.Int64).Value(1) != -2 {
		t.Fatal("value changed")
	}
	// Text where a number was declared is still an error.
	bad := record(t, []arrow.Field{{Name: "n", Type: arrow.BinaryTypes.String}}, func(b *array.RecordBuilder) {
		b.Field(0).(*array.StringBuilder).Append("x")
	})
	if _, err := Convert(bad, schema); err == nil || !strings.Contains(err.Error(), "changed type") {
		t.Fatalf("%v", err)
	}
}

func TestWideDecimalsAndIntervalsBecomeExactText(t *testing.T) {
	wide := &arrow.Decimal256Type{Precision: 76, Scale: 38}
	fits := &arrow.Decimal256Type{Precision: 38, Scale: 9}
	rec := record(t, []arrow.Field{
		{Name: "big", Type: wide}, {Name: "fits", Type: fits},
		{Name: "iv", Type: arrow.FixedWidthTypes.MonthDayNanoInterval},
	}, func(b *array.RecordBuilder) {
		n, _ := decimal256.FromString("123456789012345678901234567890.12345678901234567890123456789", 76, 38)
		b.Field(0).(*array.Decimal256Builder).Append(n)
		f, _ := decimal256.FromString("12.5", 38, 9)
		b.Field(1).(*array.Decimal256Builder).Append(f)
		b.Field(2).(*array.MonthDayNanoIntervalBuilder).Append(arrow.MonthDayNanoInterval{Months: 14, Days: 3, Nanoseconds: int64(4*time.Hour + 1500*time.Millisecond)})
	})
	out := convert(t, rec)
	if s := strs(t, out.Column(0)); s[0] != "123456789012345678901234567890.12345678901234567890123456789" {
		t.Fatalf("%q", s)
	}
	if d, ok := out.Column(1).(*array.Decimal128); !ok || d.Value(0).ToString(9) != "12.500000000" {
		t.Fatalf("%s", out.Column(1).DataType())
	}
	if s := strs(t, out.Column(2)); s[0] != "P1Y2M3DT4H1.5S" {
		t.Fatalf("%q", s)
	}
}

func TestCompactJSONKeepsKeyOrderAndNumbers(t *testing.T) {
	b := array.NewStringBuilder(mem)
	b.AppendValues([]string{"{\n  \"b\": 1.50,\n  \"a\": [\n    1,\n    2\n  ]\n}", "not json", ""}, []bool{true, true, false})
	in := b.NewStringArray()
	out := CompactJSON(in).(*array.String)
	if out.Value(0) != `{"b":1.50,"a":[1,2]}` || out.Value(1) != "not json" || !out.IsNull(2) {
		t.Fatalf("%q %q", out.Value(0), out.Value(1))
	}
}

func TestDecimalsAtOtherScalesAreRescaled(t *testing.T) {
	want := &arrow.Decimal128Type{Precision: 38, Scale: 2}
	schema := arrow.NewSchema([]arrow.Field{{Name: "d", Type: want}}, nil)
	ints := record(t, []arrow.Field{{Name: "d", Type: arrow.PrimitiveTypes.Int16}}, func(b *array.RecordBuilder) {
		b.Field(0).(*array.Int16Builder).Append(7)
	})
	out, err := Convert(ints, schema)
	if err != nil {
		t.Fatal(err)
	}
	if v := out.Column(0).(*array.Decimal128).Value(0).ToString(2); v != "7.00" {
		t.Fatal(v)
	}
}
