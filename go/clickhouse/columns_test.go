package main

import (
	"math"
	"math/big"
	"net"
	"testing"
	"time"

	"github.com/ClickHouse/clickhouse-go/v2/lib/chcol"
	"github.com/apache/arrow-go/v18/arrow"
	"github.com/google/uuid"
	"github.com/paulmach/orb"
	"github.com/shopspring/decimal"
)

func TestDeclaredTypes(t *testing.T) {
	for typ, want := range map[string]string{
		"Int8":                                "int64",
		"Nullable(UInt32)":                    "int64",
		"UInt64":                              "decimal(20, 0)",
		"Int128":                              "utf8",
		"UInt256":                             "utf8",
		"Decimal(18, 3)":                      "decimal(18, 3)",
		"Decimal(76, 40)":                     "utf8",
		"Float32":                             "float32",
		"BFloat16":                            "float32",
		"Float64":                             "float64",
		"Bool":                                "bool",
		"LowCardinality(Nullable(String))":    "utf8",
		"FixedString(4)":                      "utf8",
		"Date":                                "date32",
		"Date32":                              "date32",
		"DateTime":                            "timestamp[us, tz=UTC]",
		"DateTime('Asia/Tokyo')":              "timestamp[us, tz=UTC]",
		"DateTime64(3, 'UTC')":                "timestamp[us, tz=UTC]",
		"DateTime64(9)":                       "timestamp[ns, tz=UTC]",
		"Time":                                "utf8",
		"Array(UInt8)":                        "utf8",
		"Map(String, UInt8)":                  "utf8",
		"Enum8('a' = 1)":                      "utf8",
		"UUID":                                "utf8",
		"IPv6":                                "utf8",
		"Nothing":                             "utf8",
		"SimpleAggregateFunction(sum, UInt8)": "int64",
	} {
		if got := newColumn("c", typ).arrow.String(); got != want {
			t.Errorf("%s: %s, want %s", typ, got, want)
		}
	}
}

func TestScalarValues(t *testing.T) {
	tokyo, _ := time.LoadLocation("Asia/Tokyo")
	s := "x"
	var nothing *string
	for _, c := range []struct {
		typ  string
		v    any
		want string
	}{
		{"UInt64", uint64(math.MaxUint64), "18446744073709551615"},
		{"Int128", big.NewInt(-5), "-5"},
		{"Decimal(18, 3)", decimal.RequireFromString("12.345"), "12.345"},
		{"Decimal(18, 3)", decimal.RequireFromString("-0.5"), "-0.5"},
		{"Decimal(76, 40)", decimal.RequireFromString("1.5"), "1.5000000000000000000000000000000000000000"},
		{"FixedString(4)", "ab\x00\x00", "ab"},
		{"FixedString(2)", "\xff\x00", "ff00"},
		{"Nullable(String)", &s, "x"},
		{"Nullable(String)", nothing, "(null)"},
		{"DateTime('Asia/Tokyo')", time.Date(2026, 1, 2, 3, 4, 5, 0, tokyo), "2026-01-01T18:04:05Z"},
		{"DateTime64(9, 'UTC')", time.Date(2026, 1, 2, 3, 4, 5, 123456789, time.UTC), "2026-01-02T03:04:05.123456789Z"},
		{"Date32", time.Date(1900, 1, 1, 0, 0, 0, 0, time.UTC), "1900-01-01"},
		{"Time", 45296*time.Second + 5*time.Millisecond, "12:34:56.005"},
		{"Time", -(100*time.Hour + time.Second), "-100:00:01"},
		{"UUID", uuid.MustParse("61f0c404-5cb3-11e7-907b-a6006ad3dba0"), "61f0c404-5cb3-11e7-907b-a6006ad3dba0"},
		{"IPv4", net.IPv4(1, 2, 3, 4).To4(), "1.2.3.4"},
		{"Enum8('e1' = 1)", "e1", "e1"},
		{"IntervalDay", "3 Days", "3 Days"},
		{"Float64", math.NaN(), "NaN"},
		{"Bool", true, "true"},
	} {
		col := newColumn("c", c.typ)
		b := col.builder()
		col.append(b, c.v)
		a := b.NewArray()
		if got := a.ValueStr(0); got != c.want {
			t.Errorf("%s %#v: %q, want %q", c.typ, c.v, got, c.want)
		}
		if !arrow.TypeEqual(a.DataType(), col.arrow) {
			t.Errorf("%s: built %s, declared %s", c.typ, a.DataType(), col.arrow)
		}
	}
}

func TestNestedValuesBecomeCompactJSON(t *testing.T) {
	one := int64(1)
	js := chcol.NewJSON()
	js.SetValueAtPath("a.b", chcol.NewVariantWithType([]*int64{&one, nil}, "Array(Nullable(Int64))"))
	js.SetValueAtPath("a.c", chcol.NewVariant("x"))
	for _, c := range []struct {
		typ  string
		v    any
		want string
	}{
		{"Array(UInt8)", []uint8{1, 2}, "[1,2]"},
		{"Array(Nullable(String))", []*string{nil}, "[null]"},
		{"Tuple(UInt8, String)", []any{uint8(1), "a\"b"}, `[1,"a\"b"]`},
		{"Tuple(n UInt8, `s t` String)", map[string]any{"s t": "a", "n": uint8(1)}, `{"n":1,"s t":"a"}`},
		{"Map(String, Array(Date))", map[string][]time.Time{"k": {time.Date(2026, 1, 2, 0, 0, 0, 0, time.UTC)}, "a": nil},
			`{"a":[],"k":["2026-01-02"]}`},
		{"Map(UInt8, Float64)", map[uint8]float64{2: math.Inf(1), 1: 0.5}, `{"1":0.5,"2":"Infinity"}`},
		{"Array(DateTime64(3, 'Asia/Tokyo'))", []time.Time{time.Date(2026, 1, 2, 9, 0, 0, 5e6, time.FixedZone("JST", 9*3600))},
			`["2026-01-02T00:00:00.005Z"]`},
		{"JSON", js, `{"a":{"b":[1,null],"c":"x"}}`},
		{"Variant(String, UInt64)", chcol.NewVariant(uint64(7)), "7"},
		{"Dynamic", chcol.NewVariant(nil), "null"},
		{"Array(Decimal(10, 2))", []decimal.Decimal{decimal.RequireFromString("1.5")}, "[1.50]"},
		{"Array(UInt256)", []*big.Int{big.NewInt(9)}, "[9]"},
		{"Point", orb.Point{1, 2.5}, "[1,2.5]"},
		{"Array(UUID)", []uuid.UUID{uuid.Nil}, `["00000000-0000-0000-0000-000000000000"]`},
		{"Array(FixedString(3))", []string{"a\x00\x00"}, `["a"]`},
	} {
		col := newColumn("c", c.typ)
		b := col.builder()
		col.append(b, c.v)
		if got := b.NewArray().ValueStr(0); got != c.want {
			t.Errorf("%s: %s, want %s", c.typ, got, c.want)
		}
	}
}

func TestTypeArgumentsSplitAtTopLevel(t *testing.T) {
	name, args := splitType("Tuple(a Map(String, Array(UInt8)), `b,c` Enum8('x,y' = 1), DateTime64(3, 'UTC'))")
	if name != "Tuple" || len(args) != 3 || args[1] != "`b,c` Enum8('x,y' = 1)" || args[2] != "DateTime64(3, 'UTC')" {
		t.Fatalf("%s %q", name, args)
	}
	if n, typ := tupleElement("`b,c` Enum8('x,y' = 1)"); n != "b,c" || typ != "Enum8('x,y' = 1)" {
		t.Fatalf("%q %q", n, typ)
	}
	if n, typ := tupleElement("Array(UInt8)"); n != "" || typ != "Array(UInt8)" {
		t.Fatalf("%q %q", n, typ)
	}
}
