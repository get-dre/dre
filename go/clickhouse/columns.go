package main

// ClickHouse's column types onto DRE's type rule (docs/plugins.md):
//
//   - Nullable(T), LowCardinality(T) and SimpleAggregateFunction(f, T) read as T.
//   - Integers up to 64 bits stay integers; UInt64 becomes Decimal(20, 0), so it stays exact;
//     128- and 256-bit integers become exact text (Int128 already needs 39 digits).
//   - Decimal(P, S) stays a decimal up to 38 digits; wider becomes exact text.
//   - Date and Date32 become dates; DateTime and DateTime64 become UTC timestamps (ClickHouse
//     stores instants), in microseconds, or nanoseconds for DateTime64(7..9).
//   - Array, Tuple, Map, Nested, JSON, Variant, Dynamic and the geo types become compact,
//     single-line JSON text.
//   - Everything else becomes text: Enum names, UUID, IPv4, IPv6, intervals, Time (it can pass 24
//     hours), FixedString (NUL padding trimmed; hex when it isn't UTF-8).
//
// The driver scans each value into a Go value; append puts it in its column's Arrow builder.

import (
	"bytes"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"math"
	"math/big"
	"net"
	"reflect"
	"sort"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/ClickHouse/clickhouse-go/v2/lib/chcol"
	"github.com/apache/arrow-go/v18/arrow"
	"github.com/apache/arrow-go/v18/arrow/array"
	"github.com/apache/arrow-go/v18/arrow/decimal128"
	"github.com/apache/arrow-go/v18/arrow/memory"
	"github.com/shopspring/decimal"
)

var mem = memory.DefaultAllocator

type kind int

const (
	kText kind = iota
	kInt
	kUInt64
	kBigInt
	kDecimal
	kWideDecimal
	kFloat32
	kFloat64
	kBool
	kFixedString
	kDate
	kDateTime
	kTime
	kJSON
)

// column is one result column: its ClickHouse type, how it's read, and its Arrow type.
type column struct {
	name, typ string // typ with the Nullable/LowCardinality wrappers removed
	kind      kind
	scale     int32
	arrow     arrow.DataType
}

func newColumn(name, typ string) column {
	c := column{name: name, typ: unwrap(typ)}
	base, args := splitType(c.typ)
	switch base {
	case "Int8", "Int16", "Int32", "Int64", "UInt8", "UInt16", "UInt32":
		c.kind, c.arrow = kInt, arrow.PrimitiveTypes.Int64
	case "UInt64":
		c.kind, c.arrow = kUInt64, &arrow.Decimal128Type{Precision: 20, Scale: 0}
	case "Int128", "UInt128", "Int256", "UInt256":
		c.kind, c.arrow = kBigInt, arrow.BinaryTypes.String
	case "Decimal", "Decimal32", "Decimal64", "Decimal128", "Decimal256":
		p, s := decimalPrecision(base, args)
		c.scale = int32(s)
		if p <= 38 {
			c.kind, c.arrow = kDecimal, &arrow.Decimal128Type{Precision: int32(p), Scale: int32(s)}
		} else {
			c.kind, c.arrow = kWideDecimal, arrow.BinaryTypes.String
		}
	case "Float32", "BFloat16":
		c.kind, c.arrow = kFloat32, arrow.PrimitiveTypes.Float32
	case "Float64":
		c.kind, c.arrow = kFloat64, arrow.PrimitiveTypes.Float64
	case "Bool", "Boolean":
		c.kind, c.arrow = kBool, arrow.FixedWidthTypes.Boolean
	case "FixedString":
		c.kind, c.arrow = kFixedString, arrow.BinaryTypes.String
	case "Date", "Date32":
		c.kind, c.arrow = kDate, arrow.FixedWidthTypes.Date32
	case "DateTime", "DateTime32", "DateTime64":
		unit := arrow.Microsecond
		if base == "DateTime64" && len(args) > 0 {
			if p, _ := strconv.Atoi(args[0]); p > 6 {
				unit = arrow.Nanosecond
			}
		}
		c.kind, c.arrow = kDateTime, &arrow.TimestampType{Unit: unit, TimeZone: "UTC"}
	case "Time", "Time64":
		c.kind, c.arrow = kTime, arrow.BinaryTypes.String
	case "Array", "Tuple", "Map", "Nested", "JSON", "Object", "Variant", "Dynamic",
		"Point", "Ring", "LineString", "MultiLineString", "Polygon", "MultiPolygon", "Geometry":
		c.kind, c.arrow = kJSON, arrow.BinaryTypes.String
	default:
		c.kind, c.arrow = kText, arrow.BinaryTypes.String
	}
	return c
}

// decimalPrecision reads Decimal(P, S) and the sized forms Decimal64(S).
func decimalPrecision(base string, args []string) (p, s int) {
	if base == "Decimal" {
		if len(args) > 0 {
			p, _ = strconv.Atoi(args[0])
		}
		if len(args) > 1 {
			s, _ = strconv.Atoi(args[1])
		}
		return p, s
	}
	if len(args) > 0 {
		s, _ = strconv.Atoi(args[0])
	}
	return map[string]int{"Decimal32": 9, "Decimal64": 18, "Decimal128": 38, "Decimal256": 76}[base], s
}

// unwrap removes the wrappers that don't change what a value is.
func unwrap(typ string) string {
	for {
		base, args := splitType(typ)
		switch {
		case (base == "Nullable" || base == "LowCardinality") && len(args) == 1:
			typ = args[0]
		case base == "SimpleAggregateFunction" && len(args) == 2:
			typ = args[1]
		default:
			return typ
		}
	}
}

// splitType splits `Name(arg, arg, ...)` at its top-level commas, minding nested parentheses,
// quotes and backquotes.
func splitType(typ string) (string, []string) {
	typ = strings.TrimSpace(typ)
	open := strings.IndexByte(typ, '(')
	if open < 0 || !strings.HasSuffix(typ, ")") {
		return typ, nil
	}
	var args []string
	depth, start := 0, open+1
	var quote byte
	body := typ[:len(typ)-1]
	for i := open + 1; i < len(body); i++ {
		ch := body[i]
		switch {
		case quote != 0:
			if ch == '\\' {
				i++
			} else if ch == quote {
				quote = 0
			}
		case ch == '\'' || ch == '`' || ch == '"':
			quote = ch
		case ch == '(':
			depth++
		case ch == ')':
			depth--
		case ch == ',' && depth == 0:
			args = append(args, strings.TrimSpace(body[start:i]))
			start = i + 1
		}
	}
	if rest := strings.TrimSpace(body[start:]); rest != "" {
		args = append(args, rest)
	}
	return typ[:open], args
}

// tupleElement splits a Tuple element into its name (empty when unnamed) and type.
func tupleElement(el string) (name, typ string) {
	if strings.HasPrefix(el, "`") {
		if end := strings.Index(el[1:], "`"); end >= 0 {
			return strings.ReplaceAll(el[1:end+1], "``", "`"), strings.TrimSpace(el[end+2:])
		}
	}
	sp, paren := strings.IndexByte(el, ' '), strings.IndexByte(el, '(')
	if sp > 0 && (paren < 0 || sp < paren) {
		return el[:sp], strings.TrimSpace(el[sp+1:])
	}
	return "", el
}

func (c column) builder() array.Builder { return array.NewBuilder(mem, c.arrow) }

// append adds one scanned value (a pointer's target, or a pointer for Nullable) to b.
func (c column) append(b array.Builder, v any) {
	v = deref(v)
	if v == nil {
		b.AppendNull()
		return
	}
	switch c.kind {
	case kInt:
		if n, ok := toInt64(v); ok {
			b.(*array.Int64Builder).Append(n)
			return
		}
	case kUInt64:
		if n, ok := v.(uint64); ok {
			b.(*array.Decimal128Builder).Append(decimal128.FromU64(n))
			return
		}
	case kDecimal:
		if d, ok := v.(decimal.Decimal); ok {
			b.(*array.Decimal128Builder).Append(decimal128.FromBigInt(d.Shift(c.scale).BigInt()))
			return
		}
	case kWideDecimal:
		if d, ok := v.(decimal.Decimal); ok {
			b.(*array.StringBuilder).Append(d.StringFixed(c.scale))
			return
		}
	case kFloat32:
		if f, ok := v.(float32); ok {
			b.(*array.Float32Builder).Append(f)
			return
		}
	case kFloat64:
		if f, ok := v.(float64); ok {
			b.(*array.Float64Builder).Append(f)
			return
		}
	case kBool:
		if x, ok := v.(bool); ok {
			b.(*array.BooleanBuilder).Append(x)
			return
		}
	case kDate:
		if t, ok := v.(time.Time); ok {
			b.(*array.Date32Builder).Append(arrow.Date32(daysSinceEpoch(t)))
			return
		}
	case kDateTime:
		if t, ok := v.(time.Time); ok {
			if c.arrow.(*arrow.TimestampType).Unit == arrow.Nanosecond {
				b.(*array.TimestampBuilder).Append(arrow.Timestamp(t.UnixNano()))
			} else {
				b.(*array.TimestampBuilder).Append(arrow.Timestamp(t.UnixMicro()))
			}
			return
		}
	case kJSON:
		var buf bytes.Buffer
		appendJSON(&buf, v, c.typ)
		b.(*array.StringBuilder).Append(buf.String())
		return
	}
	b.(*array.StringBuilder).Append(textValue(v, c.typ))
}

func daysSinceEpoch(t time.Time) int64 {
	y, m, d := t.Date()
	return time.Date(y, m, d, 0, 0, 0, 0, time.UTC).Unix() / 86400
}

// deref follows pointers and interfaces; nil for a nil one.
func deref(v any) any {
	for {
		if v == nil {
			return nil
		}
		r := reflect.ValueOf(v)
		if r.Kind() != reflect.Pointer && r.Kind() != reflect.Interface {
			return v
		}
		if r.IsNil() {
			return nil
		}
		switch v.(type) {
		case *big.Int, *chcol.JSON:
			return v
		}
		v = r.Elem().Interface()
	}
}

func toInt64(v any) (int64, bool) {
	r := reflect.ValueOf(v)
	switch r.Kind() {
	case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64:
		return r.Int(), true
	case reflect.Uint8, reflect.Uint16, reflect.Uint32:
		return int64(r.Uint()), true
	}
	return 0, false
}

// textValue is a scalar as text, by its ClickHouse type where that matters.
func textValue(v any, typ string) string {
	base, _ := splitType(typ)
	switch x := v.(type) {
	case string:
		if base == "FixedString" {
			return fixedString(x)
		}
		return x
	case time.Duration:
		return clockTime(x)
	case time.Time:
		if base == "Date" || base == "Date32" {
			return x.Format("2006-01-02")
		}
		return x.UTC().Format(time.RFC3339Nano)
	case *big.Int:
		return x.String()
	case decimal.Decimal:
		return x.String()
	case net.IP:
		return x.String()
	case float32:
		return floatText(float64(x), 32)
	case float64:
		return floatText(x, 64)
	case fmt.Stringer:
		return x.String()
	}
	return fmt.Sprint(v)
}

// fixedString drops a FixedString's NUL padding; a value that still isn't clean text is hex.
func fixedString(s string) string {
	trimmed := strings.TrimRight(s, "\x00")
	if utf8.ValidString(trimmed) && !strings.ContainsRune(trimmed, 0) {
		return trimmed
	}
	return hex.EncodeToString([]byte(s))
}

// clockTime is a Time value as ClickHouse prints it: [-]H:MM:SS[.fff], hours past 24 allowed.
func clockTime(d time.Duration) string {
	sign := ""
	if d < 0 {
		sign, d = "-", -d
	}
	h := int64(d / time.Hour)
	m := int64(d/time.Minute) % 60
	s := int64(d/time.Second) % 60
	out := fmt.Sprintf("%s%02d:%02d:%02d", sign, h, m, s)
	if frac := d % time.Second; frac != 0 {
		out += strings.TrimRight(fmt.Sprintf(".%09d", frac), "0")
	}
	return out
}

func floatText(f float64, bits int) string {
	switch {
	case math.IsNaN(f):
		return "NaN"
	case math.IsInf(f, 1):
		return "Infinity"
	case math.IsInf(f, -1):
		return "-Infinity"
	}
	return strconv.FormatFloat(f, 'g', -1, bits)
}

// appendJSON writes v, of ClickHouse type typ, as compact JSON. The type says what can't be
// told from the Go value: a Date from a midnight DateTime, a named Tuple's field order.
func appendJSON(buf *bytes.Buffer, v any, typ string) {
	v = deref(v)
	typ = unwrap(typ)
	base, args := splitType(typ)
	if v == nil {
		buf.WriteString("null")
		return
	}
	switch x := v.(type) {
	case *chcol.JSON:
		appendJSON(buf, x.NestedMap(), "")
		return
	case chcol.JSON:
		appendJSON(buf, x.NestedMap(), "")
		return
	case chcol.Variant:
		appendJSON(buf, x.Any(), x.Type())
		return
	case bool:
		buf.WriteString(strconv.FormatBool(x))
		return
	case float32:
		appendJSONFloat(buf, float64(x), 32)
		return
	case float64:
		appendJSONFloat(buf, x, 64)
		return
	case *big.Int:
		buf.WriteString(x.String())
		return
	case decimal.Decimal:
		if strings.HasPrefix(base, "Decimal") {
			_, s := decimalPrecision(base, args)
			buf.WriteString(x.StringFixed(int32(s)))
		} else {
			buf.WriteString(x.String())
		}
		return
	case string, time.Time, time.Duration, net.IP:
		appendJSONString(buf, textValue(x, typ))
		return
	}
	r := reflect.ValueOf(v)
	switch r.Kind() {
	case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64:
		buf.WriteString(strconv.FormatInt(r.Int(), 10))
	case reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64:
		buf.WriteString(strconv.FormatUint(r.Uint(), 10))
	case reflect.Slice, reflect.Array:
		if s, ok := v.(fmt.Stringer); ok && r.Kind() == reflect.Array && r.Type().Elem().Kind() == reflect.Uint8 {
			appendJSONString(buf, s.String()) // UUID
			return
		}
		buf.WriteByte('[')
		for i := 0; i < r.Len(); i++ {
			if i > 0 {
				buf.WriteByte(',')
			}
			appendJSON(buf, r.Index(i).Interface(), elementType(base, args, i))
		}
		buf.WriteByte(']')
	case reflect.Map:
		appendJSONMap(buf, r, base, args)
	default:
		if s, ok := v.(fmt.Stringer); ok {
			appendJSONString(buf, s.String())
		} else {
			appendJSONString(buf, fmt.Sprint(v))
		}
	}
}

// elementType is the type of element i of an Array or unnamed Tuple.
func elementType(base string, args []string, i int) string {
	switch {
	case base == "Array" && len(args) == 1:
		return args[0]
	case base == "Tuple" && i < len(args):
		_, t := tupleElement(args[i])
		return t
	}
	return ""
}

// appendJSONMap writes a Map, a named Tuple or a JSON object: keys as text, a named Tuple's in
// its declared order and everything else's sorted.
func appendJSONMap(buf *bytes.Buffer, r reflect.Value, base string, args []string) {
	type entry struct {
		key string
		val any
		typ string
	}
	var entries []entry
	if base == "Tuple" {
		for _, a := range args {
			name, t := tupleElement(a)
			if v := r.MapIndex(reflect.ValueOf(name)); v.IsValid() {
				entries = append(entries, entry{name, v.Interface(), t})
			}
		}
	} else {
		valType := ""
		if base == "Map" && len(args) == 2 {
			valType = args[1]
		}
		it := r.MapRange()
		for it.Next() {
			entries = append(entries, entry{textValue(deref(it.Key().Interface()), ""), it.Value().Interface(), valType})
		}
		sort.Slice(entries, func(i, j int) bool { return entries[i].key < entries[j].key })
	}
	buf.WriteByte('{')
	for i, e := range entries {
		if i > 0 {
			buf.WriteByte(',')
		}
		appendJSONString(buf, e.key)
		buf.WriteByte(':')
		appendJSON(buf, e.val, e.typ)
	}
	buf.WriteByte('}')
}

// appendJSONFloat writes NaN and the infinities, which JSON can't hold, as strings (the shared
// rule).
func appendJSONFloat(buf *bytes.Buffer, f float64, bits int) {
	if math.IsNaN(f) || math.IsInf(f, 0) {
		appendJSONString(buf, floatText(f, bits))
		return
	}
	buf.WriteString(strconv.FormatFloat(f, 'g', -1, bits))
}

func appendJSONString(buf *bytes.Buffer, s string) {
	enc := json.NewEncoder(buf)
	enc.SetEscapeHTML(false)
	_ = enc.Encode(s)
	buf.Truncate(buf.Len() - 1)
}
