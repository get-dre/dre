package plugin

// DRE's type rule for every warehouse source (docs/plugins.md):
//
//   - scalars keep their type;
//   - semi-structured and nested values (struct, list, map, union) become compact, single-line
//     JSON text;
//   - types no format can hold exactly (intervals, durations, decimals wider than 38 digits)
//     become text;
//   - NULL-typed columns (SELECT NULL) become text.
//
// OutputSchema applies the rule to a result's declared schema and Convert puts every batch
// under it, which also evens out batches whose integer or decimal sizes vary (Snowflake sizes
// integers per batch).

import (
	"bytes"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"math"
	"strconv"
	"strings"
	"time"

	"github.com/apache/arrow-go/v18/arrow"
	"github.com/apache/arrow-go/v18/arrow/array"
	"github.com/apache/arrow-go/v18/arrow/decimal128"
	"github.com/apache/arrow-go/v18/arrow/memory"
)

// OutputSchema is the schema sent to core for a result whose columns are declared as s: the
// type rule applied, without driver metadata, every column nullable.
func OutputSchema(s *arrow.Schema) *arrow.Schema {
	fields := make([]arrow.Field, len(s.Fields()))
	for i, f := range s.Fields() {
		fields[i] = arrow.Field{Name: f.Name, Type: outputType(f.Type), Nullable: true}
	}
	return arrow.NewSchema(fields, nil)
}

func outputType(t arrow.DataType) arrow.DataType {
	switch t.ID() {
	case arrow.NULL, arrow.STRUCT, arrow.LIST, arrow.LARGE_LIST, arrow.LIST_VIEW, arrow.LARGE_LIST_VIEW,
		arrow.FIXED_SIZE_LIST, arrow.MAP, arrow.SPARSE_UNION, arrow.DENSE_UNION,
		arrow.INTERVAL_MONTHS, arrow.INTERVAL_DAY_TIME, arrow.INTERVAL_MONTH_DAY_NANO, arrow.DURATION:
		return arrow.BinaryTypes.String
	case arrow.DECIMAL256:
		d := t.(*arrow.Decimal256Type)
		if d.Precision <= 38 {
			return &arrow.Decimal128Type{Precision: d.Precision, Scale: d.Scale}
		}
		return arrow.BinaryTypes.String
	case arrow.DICTIONARY:
		return outputType(t.(*arrow.DictionaryType).ValueType)
	}
	return t
}

// Convert puts rec's columns under schema (from OutputSchema). The caller releases the result.
func Convert(rec arrow.Record, schema *arrow.Schema) (arrow.Record, error) {
	if int(rec.NumCols()) != len(schema.Fields()) {
		return nil, fmt.Errorf("a batch has %d columns but the result has %d", rec.NumCols(), len(schema.Fields()))
	}
	cols := make([]arrow.Array, rec.NumCols())
	for i := range cols {
		c, err := convertColumn(rec.Column(i), schema.Field(i))
		if err != nil {
			for _, done := range cols[:i] {
				done.Release()
			}
			return nil, err
		}
		cols[i] = c
	}
	out := array.NewRecord(schema, cols, rec.NumRows())
	for _, c := range cols {
		c.Release()
	}
	return out, nil
}

func convertColumn(c arrow.Array, f arrow.Field) (arrow.Array, error) {
	want, got := f.Type, c.DataType()
	if arrow.TypeEqual(got, want) {
		c.Retain()
		return c, nil
	}
	mem := memory.DefaultAllocator
	if got.ID() == arrow.NULL {
		return array.MakeArrayOfNull(mem, want, c.Len()), nil
	}
	changed := func() error {
		return fmt.Errorf("column `%s` changed type from %s to %s between batches", f.Name, want, got)
	}
	switch want.ID() {
	case arrow.STRING:
		b := array.NewStringBuilder(mem)
		defer b.Release()
		var buf bytes.Buffer
		for i := 0; i < c.Len(); i++ {
			if c.IsNull(i) {
				b.AppendNull()
				continue
			}
			buf.Reset()
			if isNested(got) {
				appendJSON(&buf, c, i)
			} else {
				buf.WriteString(text(c, i))
			}
			b.Append(buf.String())
		}
		return b.NewArray(), nil
	case arrow.INT64:
		b := array.NewInt64Builder(mem)
		defer b.Release()
		for i := 0; i < c.Len(); i++ {
			if c.IsNull(i) {
				b.AppendNull()
				continue
			}
			v, ok := intValue(c, i)
			if !ok {
				return nil, changed()
			}
			b.Append(v)
		}
		return b.NewArray(), nil
	case arrow.FLOAT64:
		b := array.NewFloat64Builder(mem)
		defer b.Release()
		for i := 0; i < c.Len(); i++ {
			if c.IsNull(i) {
				b.AppendNull()
				continue
			}
			switch a := c.(type) {
			case *array.Float32:
				b.Append(float64(a.Value(i)))
			case *array.Float16:
				b.Append(float64(a.Value(i).Float32()))
			default:
				return nil, changed()
			}
		}
		return b.NewArray(), nil
	case arrow.DECIMAL128:
		wt := want.(*arrow.Decimal128Type)
		b := array.NewDecimal128Builder(mem, wt)
		defer b.Release()
		for i := 0; i < c.Len(); i++ {
			if c.IsNull(i) {
				b.AppendNull()
				continue
			}
			v, ok := decimalValue(c, i, wt.Scale)
			if !ok {
				return nil, changed()
			}
			b.Append(v)
		}
		return b.NewArray(), nil
	case arrow.TIMESTAMP:
		wt := want.(*arrow.TimestampType)
		a, ok := c.(*array.Timestamp)
		// Zone-aware values are UTC instants whatever zone name the driver gives; only a naive
		// timestamp and a zoned one differ.
		if !ok || (a.DataType().(*arrow.TimestampType).TimeZone == "") != (wt.TimeZone == "") {
			return nil, changed()
		}
		from := a.DataType().(*arrow.TimestampType).Unit
		b := array.NewTimestampBuilder(mem, wt)
		defer b.Release()
		for i := 0; i < c.Len(); i++ {
			if c.IsNull(i) {
				b.AppendNull()
				continue
			}
			b.Append(arrow.Timestamp(rescale(int64(a.Value(i)), from, wt.Unit)))
		}
		return b.NewArray(), nil
	}
	return nil, changed()
}

func isNested(t arrow.DataType) bool {
	switch t.ID() {
	case arrow.STRUCT, arrow.LIST, arrow.LARGE_LIST, arrow.LIST_VIEW, arrow.LARGE_LIST_VIEW,
		arrow.FIXED_SIZE_LIST, arrow.MAP, arrow.SPARSE_UNION, arrow.DENSE_UNION:
		return true
	case arrow.DICTIONARY:
		return isNested(t.(*arrow.DictionaryType).ValueType)
	}
	return false
}

func intValue(c arrow.Array, i int) (int64, bool) {
	switch a := c.(type) {
	case *array.Int8:
		return int64(a.Value(i)), true
	case *array.Int16:
		return int64(a.Value(i)), true
	case *array.Int32:
		return int64(a.Value(i)), true
	case *array.Int64:
		return a.Value(i), true
	case *array.Uint8:
		return int64(a.Value(i)), true
	case *array.Uint16:
		return int64(a.Value(i)), true
	case *array.Uint32:
		return int64(a.Value(i)), true
	}
	return 0, false
}

// decimalValue is cell i as a decimal at scale; integers count as scale 0.
func decimalValue(c arrow.Array, i int, scale int32) (decimal128.Num, bool) {
	var n decimal128.Num
	var from int32
	switch a := c.(type) {
	case *array.Decimal128:
		n, from = a.Value(i), a.DataType().(*arrow.Decimal128Type).Scale
	case *array.Decimal256:
		// Only declared as 128-bit when the precision fits, so the high words are sign.
		w := a.Value(i).Array()
		n, from = decimal128.New(int64(w[1]), w[0]), a.DataType().(*arrow.Decimal256Type).Scale
	default:
		v, ok := intValue(c, i)
		if !ok {
			return n, false
		}
		n = decimal128.FromI64(v)
	}
	if from == scale {
		return n, true
	}
	if from < scale {
		return n.IncreaseScaleBy(scale - from), true
	}
	return n.ReduceScaleBy(from-scale, false), true
}

func rescale(v int64, from, to arrow.TimeUnit) int64 {
	f, t := from.Multiplier(), to.Multiplier()
	if f == t {
		return v
	}
	ns := v * int64(f)
	return ns / int64(t)
}

// CompactJSON re-encodes a column of JSON text (Snowflake's pretty-printed VARIANT, a JSON
// column) as compact, single-line JSON, keeping key order and number text. A value that
// isn't valid JSON is kept as it is.
func CompactJSON(c *array.String) arrow.Array {
	b := array.NewStringBuilder(memory.DefaultAllocator)
	defer b.Release()
	var buf bytes.Buffer
	for i := 0; i < c.Len(); i++ {
		if c.IsNull(i) {
			b.AppendNull()
			continue
		}
		buf.Reset()
		if err := json.Compact(&buf, []byte(c.Value(i))); err != nil {
			b.Append(c.Value(i))
			continue
		}
		b.Append(buf.String())
	}
	return b.NewArray()
}

// text is a scalar cell as exact text: intervals as ISO 8601 durations, wide decimals in full.
func text(c arrow.Array, i int) string {
	switch a := c.(type) {
	case *array.MonthInterval:
		return isoDuration(int64(a.Value(i)), 0, 0)
	case *array.DayTimeInterval:
		v := a.Value(i)
		return isoDuration(0, int64(v.Days), int64(v.Milliseconds)*int64(time.Millisecond))
	case *array.MonthDayNanoInterval:
		v := a.Value(i)
		return isoDuration(int64(v.Months), int64(v.Days), v.Nanoseconds)
	case *array.Duration:
		u := a.DataType().(*arrow.DurationType).Unit
		return isoDuration(0, 0, int64(a.Value(i))*int64(u.Multiplier()))
	case *array.Decimal256:
		// Too wide for a 38-digit decimal (BIGNUMERIC): exact, without trailing zeros.
		return trimZeros(a.Value(i).ToString(a.DataType().(*arrow.Decimal256Type).Scale))
	case *array.Decimal128:
		return a.Value(i).ToString(a.DataType().(*arrow.Decimal128Type).Scale)
	case *array.Dictionary:
		return text(a.Dictionary(), a.GetValueIndex(i))
	}
	var buf bytes.Buffer
	appendJSON(&buf, c, i)
	var s string
	if json.Unmarshal(buf.Bytes(), &s) == nil {
		return s
	}
	return buf.String()
}

func trimZeros(s string) string {
	if strings.Contains(s, ".") {
		s = strings.TrimRight(strings.TrimRight(s, "0"), ".")
	}
	return s
}

// isoDuration writes an interval as ISO 8601, e.g. P1Y2M3DT4H5M6.5S; P0D for zero.
func isoDuration(months, days, nanos int64) string {
	var b strings.Builder
	neg := func(v int64) int64 {
		if v < 0 {
			return -v
		}
		return v
	}
	b.WriteString("P")
	if y := months / 12; y != 0 {
		fmt.Fprintf(&b, "%dY", y)
	}
	if m := months % 12; m != 0 {
		fmt.Fprintf(&b, "%dM", m)
	}
	if days != 0 {
		fmt.Fprintf(&b, "%dD", days)
	}
	if nanos != 0 {
		b.WriteString("T")
		sign := ""
		if nanos < 0 {
			sign = "-"
		}
		n := neg(nanos)
		h, n := n/int64(time.Hour), n%int64(time.Hour)
		m, n := n/int64(time.Minute), n%int64(time.Minute)
		if h != 0 {
			fmt.Fprintf(&b, "%s%dH", sign, h)
		}
		if m != 0 {
			fmt.Fprintf(&b, "%s%dM", sign, m)
		}
		if n != 0 {
			s := strconv.FormatFloat(float64(n)/float64(time.Second), 'f', -1, 64)
			fmt.Fprintf(&b, "%s%sS", sign, s)
		}
	}
	if b.Len() == 1 {
		return "P0D"
	}
	return b.String()
}

// appendJSON writes cell i of c as compact JSON.
func appendJSON(buf *bytes.Buffer, c arrow.Array, i int) {
	if c.IsNull(i) {
		buf.WriteString("null")
		return
	}
	switch a := c.(type) {
	case *array.Boolean:
		buf.WriteString(strconv.FormatBool(a.Value(i)))
	case *array.Int8, *array.Int16, *array.Int32, *array.Int64, *array.Uint8, *array.Uint16, *array.Uint32:
		v, _ := intValue(c, i)
		buf.WriteString(strconv.FormatInt(v, 10))
	case *array.Uint64:
		buf.WriteString(strconv.FormatUint(a.Value(i), 10))
	case *array.Float16:
		appendFloat(buf, float64(a.Value(i).Float32()), 32)
	case *array.Float32:
		appendFloat(buf, float64(a.Value(i)), 32)
	case *array.Float64:
		appendFloat(buf, a.Value(i), 64)
	case *array.Decimal128:
		buf.WriteString(a.Value(i).ToString(a.DataType().(*arrow.Decimal128Type).Scale))
	case *array.Decimal256:
		buf.WriteString(a.Value(i).ToString(a.DataType().(*arrow.Decimal256Type).Scale))
	case *array.String:
		appendString(buf, a.Value(i))
	case *array.LargeString:
		appendString(buf, a.Value(i))
	case *array.StringView:
		appendString(buf, a.Value(i))
	case *array.Binary:
		appendString(buf, base64.StdEncoding.EncodeToString(a.Value(i)))
	case *array.LargeBinary:
		appendString(buf, base64.StdEncoding.EncodeToString(a.Value(i)))
	case *array.FixedSizeBinary:
		appendString(buf, base64.StdEncoding.EncodeToString(a.Value(i)))
	case *array.Date32:
		appendString(buf, a.Value(i).ToTime().Format("2006-01-02"))
	case *array.Date64:
		appendString(buf, a.Value(i).ToTime().Format("2006-01-02"))
	case *array.Timestamp:
		t := a.DataType().(*arrow.TimestampType)
		v := a.Value(i).ToTime(t.Unit)
		if t.TimeZone != "" {
			appendString(buf, v.UTC().Format(time.RFC3339Nano))
		} else {
			appendString(buf, v.Format("2006-01-02T15:04:05.999999999"))
		}
	case *array.Time32:
		appendString(buf, a.Value(i).ToTime(a.DataType().(*arrow.Time32Type).Unit).Format("15:04:05.999999999"))
	case *array.Time64:
		appendString(buf, a.Value(i).ToTime(a.DataType().(*arrow.Time64Type).Unit).Format("15:04:05.999999999"))
	case *array.Struct:
		st := a.DataType().(*arrow.StructType)
		buf.WriteByte('{')
		for f := 0; f < a.NumField(); f++ {
			if f > 0 {
				buf.WriteByte(',')
			}
			appendString(buf, st.Field(f).Name)
			buf.WriteByte(':')
			appendJSON(buf, a.Field(f), i)
		}
		buf.WriteByte('}')
	case *array.Map:
		// Before *array.List: a map is a list of key/value structs. Keys become JSON object
		// keys, as text.
		start, end := a.ValueOffsets(i)
		keys, items := a.Keys(), a.Items()
		buf.WriteByte('{')
		for j := start; j < end; j++ {
			if j > start {
				buf.WriteByte(',')
			}
			appendString(buf, text(keys, int(j)))
			buf.WriteByte(':')
			appendJSON(buf, items, int(j))
		}
		buf.WriteByte('}')
	case array.ListLike:
		start, end := a.ValueOffsets(i)
		values := a.ListValues()
		buf.WriteByte('[')
		for j := start; j < end; j++ {
			if j > start {
				buf.WriteByte(',')
			}
			appendJSON(buf, values, int(j))
		}
		buf.WriteByte(']')
	case array.Union:
		child := a.Field(a.ChildID(i))
		idx := i
		if d, ok := a.(*array.DenseUnion); ok {
			idx = int(d.ValueOffset(i))
		}
		appendJSON(buf, child, idx)
	case *array.Dictionary:
		appendJSON(buf, a.Dictionary(), a.GetValueIndex(i))
	case *array.MonthInterval, *array.DayTimeInterval, *array.MonthDayNanoInterval, *array.Duration:
		appendString(buf, text(c, i))
	default:
		appendString(buf, c.ValueStr(i))
	}
}

// appendFloat writes a float as JSON; NaN and infinities, which JSON can't hold, as strings.
func appendFloat(buf *bytes.Buffer, v float64, bits int) {
	switch {
	case math.IsNaN(v):
		buf.WriteString(`"NaN"`)
	case math.IsInf(v, 1):
		buf.WriteString(`"Infinity"`)
	case math.IsInf(v, -1):
		buf.WriteString(`"-Infinity"`)
	default:
		buf.WriteString(strconv.FormatFloat(v, 'g', -1, bits))
	}
}

func appendString(buf *bytes.Buffer, s string) {
	enc := json.NewEncoder(buf)
	enc.SetEscapeHTML(false)
	_ = enc.Encode(s)
	buf.Truncate(buf.Len() - 1) // Encode adds a newline
}
