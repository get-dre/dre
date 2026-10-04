package main

// Snowflake's column metadata says what a column is; its Arrow batches don't always agree.
// With exact numbers on, NUMBER(p,s) arrives as integers of whatever width each batch needed,
// carrying the scale only in the metadata; TIMESTAMP_TZ arrives as a UTC instant without a zone;
// VARIANT, OBJECT and ARRAY arrive as pretty-printed JSON text. normalise fixes each batch up to
// the declared type, and the shared type rule (plugin.Convert) does the rest.

import (
	"github.com/apache/arrow-go/v18/arrow"
	"github.com/apache/arrow-go/v18/arrow/array"
	"github.com/apache/arrow-go/v18/arrow/decimal128"
	"github.com/apache/arrow-go/v18/arrow/memory"

	"github.com/get-dre/dre/go/plugin"
)

var mem = memory.DefaultAllocator

// column is one result column as Snowflake describes it.
type column struct {
	name             string
	typ              string // FIXED, REAL, TEXT, TIMESTAMP_TZ, VARIANT, ...
	precision, scale int64
}

// declaredType is the Arrow type a column is sent as.
func declaredType(c column) arrow.DataType {
	switch c.typ {
	case "FIXED":
		if c.scale == 0 && c.precision > 0 && c.precision <= 18 {
			return arrow.PrimitiveTypes.Int64
		}
		p := c.precision
		if p <= 0 || p > 38 {
			p = 38
		}
		return &arrow.Decimal128Type{Precision: int32(p), Scale: int32(c.scale)}
	case "REAL":
		return arrow.PrimitiveTypes.Float64
	case "BOOLEAN":
		return arrow.FixedWidthTypes.Boolean
	case "BINARY":
		return arrow.BinaryTypes.Binary
	case "DATE":
		return arrow.FixedWidthTypes.Date32
	case "TIME":
		return arrow.FixedWidthTypes.Time64ns
	case "TIMESTAMP_NTZ":
		return &arrow.TimestampType{Unit: arrow.Microsecond}
	case "TIMESTAMP_LTZ", "TIMESTAMP_TZ":
		return &arrow.TimestampType{Unit: arrow.Microsecond, TimeZone: "UTC"}
	}
	// TEXT, and everything that becomes text: VARIANT, OBJECT, ARRAY, MAP, GEOGRAPHY,
	// GEOMETRY, VECTOR, DECFLOAT.
	return arrow.BinaryTypes.String
}

func declaredSchema(cols []column) *arrow.Schema {
	fields := make([]arrow.Field, len(cols))
	for i, c := range cols {
		fields[i] = arrow.Field{Name: c.name, Type: declaredType(c), Nullable: true}
	}
	return arrow.NewSchema(fields, nil)
}

// isJSON is true for types Snowflake sends as JSON text.
func isJSON(typ string) bool {
	switch typ {
	case "VARIANT", "OBJECT", "ARRAY", "MAP", "GEOGRAPHY", "GEOMETRY":
		return true
	}
	return false
}

// normalise brings one batch to the declared types. The caller releases the result.
func normalise(rec arrow.Record, cols []column) (arrow.Record, error) {
	out := make([]arrow.Array, rec.NumCols())
	for i := range out {
		c, col := rec.Column(i), cols[i]
		want := declaredType(col)
		switch {
		case col.typ == "FIXED" && want.ID() == arrow.DECIMAL128:
			if a := scaledDecimal(c, want.(*arrow.Decimal128Type)); a != nil {
				out[i] = a
				continue
			}
		case col.typ == "FIXED" && want.ID() == arrow.INT64:
			if d, ok := c.(*array.Decimal128); ok {
				b := array.NewInt64Builder(mem)
				for j := 0; j < d.Len(); j++ {
					if d.IsNull(j) {
						b.AppendNull()
					} else {
						b.Append(int64(d.Value(j).LowBits()))
					}
				}
				out[i] = b.NewArray()
				b.Release()
				continue
			}
		case col.typ == "TIMESTAMP_TZ" || col.typ == "TIMESTAMP_LTZ":
			if ts, ok := c.(*array.Timestamp); ok {
				// The values are UTC instants; say so.
				data := ts.Data()
				z := array.NewData(&arrow.TimestampType{Unit: data.DataType().(*arrow.TimestampType).Unit, TimeZone: "UTC"},
					data.Len(), data.Buffers(), nil, data.NullN(), data.Offset())
				out[i] = array.MakeFromData(z)
				z.Release()
				continue
			}
		case isJSON(col.typ):
			if s, ok := c.(*array.String); ok {
				out[i] = plugin.CompactJSON(s)
				continue
			}
		}
		c.Retain()
		out[i] = c
	}
	fields := make([]arrow.Field, len(out))
	for i, a := range out {
		fields[i] = arrow.Field{Name: cols[i].name, Type: a.DataType(), Nullable: true}
	}
	res := array.NewRecord(arrow.NewSchema(fields, nil), out, rec.NumRows())
	for _, a := range out {
		a.Release()
	}
	return res, nil
}

// scaledDecimal reads integers that hold NUMBER(p,s) values scaled by 10^s (how Snowflake sends
// them) as decimals; nil when c isn't such a column.
func scaledDecimal(c arrow.Array, want *arrow.Decimal128Type) arrow.Array {
	b := array.NewDecimal128Builder(mem, want)
	defer b.Release()
	for j := 0; j < c.Len(); j++ {
		if c.IsNull(j) {
			b.AppendNull()
			continue
		}
		switch a := c.(type) {
		case *array.Int8:
			b.Append(decimal128.FromI64(int64(a.Value(j))))
		case *array.Int16:
			b.Append(decimal128.FromI64(int64(a.Value(j))))
		case *array.Int32:
			b.Append(decimal128.FromI64(int64(a.Value(j))))
		case *array.Int64:
			b.Append(decimal128.FromI64(a.Value(j)))
		case *array.Decimal128:
			// Already unscaled at the column's scale.
			b.Append(a.Value(j))
		default:
			return nil
		}
	}
	return b.NewArray()
}
