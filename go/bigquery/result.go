package main

// Results reach core the same way whichever API served them. The Storage Read API sends Arrow;
// REST rows are built into the same Arrow types here (arrowType). Both then pass through the
// shared type rule (plugin.OutputSchema/Convert): STRUCT, ARRAY and RANGE become compact JSON
// text, INTERVAL becomes ISO 8601 text, BIGNUMERIC exact text. JSON columns are compacted.

import (
	"errors"
	"fmt"
	"io"
	"math/big"
	"time"

	"cloud.google.com/go/bigquery"
	"cloud.google.com/go/civil"
	"github.com/apache/arrow-go/v18/arrow"
	"github.com/apache/arrow-go/v18/arrow/array"
	"github.com/apache/arrow-go/v18/arrow/decimal128"
	"github.com/apache/arrow-go/v18/arrow/decimal256"
	"github.com/apache/arrow-go/v18/arrow/ipc"
	"github.com/apache/arrow-go/v18/arrow/memory"
	"google.golang.org/api/iterator"

	"github.com/get-dre/dre/go/plugin"
)

// batchRows is how many REST rows go into one Arrow batch.
const batchRows = 10_000

var mem = memory.DefaultAllocator

// arrowSchema is the Arrow schema the Storage Read API gives for a BigQuery schema.
func arrowSchema(s bigquery.Schema) *arrow.Schema {
	fields := make([]arrow.Field, len(s))
	for i, f := range s {
		fields[i] = arrowField(f)
	}
	return arrow.NewSchema(fields, nil)
}

func arrowField(f *bigquery.FieldSchema) arrow.Field {
	t := arrowType(f)
	if f.Repeated {
		t = arrow.ListOf(t)
	}
	return arrow.Field{Name: f.Name, Type: t, Nullable: true}
}

func arrowType(f *bigquery.FieldSchema) arrow.DataType {
	switch f.Type {
	case bigquery.IntegerFieldType:
		return arrow.PrimitiveTypes.Int64
	case bigquery.FloatFieldType:
		return arrow.PrimitiveTypes.Float64
	case bigquery.BooleanFieldType:
		return arrow.FixedWidthTypes.Boolean
	case bigquery.BytesFieldType:
		return arrow.BinaryTypes.Binary
	case bigquery.NumericFieldType:
		return &arrow.Decimal128Type{Precision: 38, Scale: 9}
	case bigquery.BigNumericFieldType:
		return &arrow.Decimal256Type{Precision: 76, Scale: 38}
	case bigquery.DateFieldType:
		return arrow.FixedWidthTypes.Date32
	case bigquery.TimeFieldType:
		return arrow.FixedWidthTypes.Time64us
	case bigquery.DateTimeFieldType:
		return &arrow.TimestampType{Unit: arrow.Microsecond}
	case bigquery.TimestampFieldType:
		return &arrow.TimestampType{Unit: arrow.Microsecond, TimeZone: "UTC"}
	case bigquery.IntervalFieldType:
		return arrow.FixedWidthTypes.MonthDayNanoInterval
	case bigquery.RecordFieldType:
		fs := make([]arrow.Field, len(f.Schema))
		for i, c := range f.Schema {
			fs[i] = arrowField(c)
		}
		return arrow.StructOf(fs...)
	case bigquery.RangeFieldType:
		el := &bigquery.FieldSchema{Type: bigquery.DateFieldType}
		if f.RangeElementType != nil {
			el.Type = f.RangeElementType.Type
		}
		return arrow.StructOf(
			arrow.Field{Name: "start", Type: arrowType(el), Nullable: true},
			arrow.Field{Name: "end", Type: arrowType(el), Nullable: true},
		)
	}
	// STRING, GEOGRAPHY (WKT) and JSON.
	return arrow.BinaryTypes.String
}

// result is one statement's rows, from either API, as Storage-API-shaped Arrow batches.
type result struct {
	bq     bigquery.Schema
	schema *arrow.Schema
	next   func() (arrow.Record, error) // io.EOF at the end
	peeked arrow.Record
	err    error
	done   bool
}

// errNoResult means the statement returned no result set (DDL, DML, SET).
var errNoResult = errors.New("no result set")

func newResult(it *bigquery.RowIterator) (*result, error) {
	if it.IsAccelerated() {
		debugf("reading the result through the Storage Read API")
		ai, err := it.ArrowIterator()
		if err != nil {
			return nil, err
		}
		bq := it.Schema
		if len(bq) == 0 {
			bq = ai.Schema()
		}
		r := &result{bq: bq, schema: arrowSchema(bq)}
		rd, err := ipc.NewReader(bigquery.NewArrowIteratorReader(ai), ipc.WithAllocator(mem))
		if err != nil {
			return nil, fmt.Errorf("can't read the Storage Read API's Arrow: %w", err)
		}
		r.next = func() (arrow.Record, error) {
			if !rd.Next() {
				if err := rd.Err(); err != nil && !errors.Is(err, io.EOF) {
					return nil, err
				}
				rd.Release()
				return nil, io.EOF
			}
			rec := rd.Record()
			rec.Retain()
			return rec, nil
		}
		return r, nil
	}
	// Over REST the schema is only known once the first page is read, so read the first row
	// now; a statement without a result set has no schema.
	var first []bigquery.Value
	err := it.Next(&first)
	if err != nil && err != iterator.Done {
		return nil, err
	}
	if len(it.Schema) == 0 {
		return nil, errNoResult
	}
	debugf("reading the result through the REST API")
	r := &result{bq: it.Schema, schema: arrowSchema(it.Schema)}
	pending := err == nil
	next := func(row *[]bigquery.Value) error {
		if pending {
			*row, pending = first, false
			return nil
		}
		return it.Next(row)
	}
	r.next = func() (arrow.Record, error) { return restBatch(next, r.bq, r.schema, batchRows) }
	return r, nil
}

func (r *result) Schema() (*arrow.Schema, error) { return r.schema, nil }

func (r *result) HasNext() bool {
	if r.peeked != nil || r.err != nil {
		return true
	}
	if r.done {
		return false
	}
	rec, err := r.next()
	switch {
	case err == io.EOF:
		r.done = true
		return false
	case err != nil:
		r.err = err
	default:
		r.peeked = rec
	}
	return true
}

func (r *result) Next() (arrow.Record, error) {
	if !r.HasNext() {
		return nil, io.EOF
	}
	if r.err != nil {
		err := r.err
		r.err, r.done = nil, true
		return nil, cleanErr(err)
	}
	rec := r.peeked
	r.peeked = nil
	defer rec.Release()
	return r.normalise(rec)
}

// normalise compacts JSON columns and turns interval text (Storage API) into intervals.
func (r *result) normalise(rec arrow.Record) (arrow.Record, error) {
	cols := make([]arrow.Array, rec.NumCols())
	for i := range cols {
		c := rec.Column(i)
		f := r.bq[i]
		switch {
		case f.Type == bigquery.JSONFieldType && !f.Repeated:
			if s, ok := c.(*array.String); ok {
				cols[i] = plugin.CompactJSON(s)
				continue
			}
		case f.Type == bigquery.IntervalFieldType && !f.Repeated:
			if s, ok := c.(*array.String); ok {
				iv, err := intervalsFromText(s)
				if err != nil {
					return nil, err
				}
				cols[i] = iv
				continue
			}
		}
		c.Retain()
		cols[i] = c
	}
	fields := make([]arrow.Field, len(cols))
	for i, c := range cols {
		fields[i] = arrow.Field{Name: rec.Schema().Field(i).Name, Type: c.DataType(), Nullable: true}
	}
	out := array.NewRecord(arrow.NewSchema(fields, nil), cols, rec.NumRows())
	for _, c := range cols {
		c.Release()
	}
	return out, nil
}

func intervalsFromText(s *array.String) (arrow.Array, error) {
	b := array.NewMonthDayNanoIntervalBuilder(mem)
	defer b.Release()
	for i := 0; i < s.Len(); i++ {
		if s.IsNull(i) {
			b.AppendNull()
			continue
		}
		v, err := bigquery.ParseInterval(s.Value(i))
		if err != nil {
			return nil, fmt.Errorf("can't read the interval %q: %v", s.Value(i), err)
		}
		b.Append(monthDayNano(v))
	}
	return b.NewArray(), nil
}

func monthDayNano(v *bigquery.IntervalValue) arrow.MonthDayNanoInterval {
	nanos := int64(v.Hours)*int64(time.Hour) + int64(v.Minutes)*int64(time.Minute) +
		int64(v.Seconds)*int64(time.Second) + int64(v.SubSecondNanos)
	return arrow.MonthDayNanoInterval{Months: v.Years*12 + v.Months, Days: v.Days, Nanoseconds: nanos}
}

// restBatch reads up to n REST rows (from next, which returns iterator.Done at the end) into
// one Arrow batch; io.EOF when there are none left.
func restBatch(next func(*[]bigquery.Value) error, bq bigquery.Schema, schema *arrow.Schema, n int) (arrow.Record, error) {
	b := array.NewRecordBuilder(mem, schema)
	defer b.Release()
	rows := 0
	for rows < n {
		var row []bigquery.Value
		err := next(&row)
		if err == iterator.Done {
			break
		}
		if err != nil {
			return nil, err
		}
		for i, f := range bq {
			if err := appendValue(b.Field(i), f, row[i], true); err != nil {
				return nil, fmt.Errorf("column `%s`: %v", f.Name, err)
			}
		}
		rows++
	}
	if rows == 0 {
		return nil, io.EOF
	}
	return b.NewRecord(), nil
}

// appendValue appends one REST value; top says whether f's REPEATED applies at this level.
func appendValue(b array.Builder, f *bigquery.FieldSchema, v bigquery.Value, top bool) error {
	if v == nil {
		b.AppendNull()
		return nil
	}
	if top && f.Repeated {
		lb := b.(*array.ListBuilder)
		items, ok := v.([]bigquery.Value)
		if !ok {
			return fmt.Errorf("expected an array, got %T", v)
		}
		lb.Append(true)
		for _, item := range items {
			if err := appendValue(lb.ValueBuilder(), f, item, false); err != nil {
				return err
			}
		}
		return nil
	}
	switch bb := b.(type) {
	case *array.Int64Builder:
		x, ok := v.(int64)
		if !ok {
			return fmt.Errorf("expected an integer, got %T", v)
		}
		bb.Append(x)
	case *array.Float64Builder:
		x, ok := v.(float64)
		if !ok {
			return fmt.Errorf("expected a float, got %T", v)
		}
		bb.Append(x)
	case *array.BooleanBuilder:
		x, ok := v.(bool)
		if !ok {
			return fmt.Errorf("expected a boolean, got %T", v)
		}
		bb.Append(x)
	case *array.BinaryBuilder:
		x, ok := v.([]byte)
		if !ok {
			return fmt.Errorf("expected bytes, got %T", v)
		}
		bb.Append(x)
	case *array.StringBuilder:
		switch x := v.(type) {
		case string:
			bb.Append(x)
		default:
			bb.Append(fmt.Sprint(x))
		}
	case *array.Decimal128Builder:
		r, ok := v.(*big.Rat)
		if !ok {
			return fmt.Errorf("expected a NUMERIC, got %T", v)
		}
		n, err := decimal128.FromString(r.FloatString(9), 38, 9)
		if err != nil {
			return err
		}
		bb.Append(n)
	case *array.Decimal256Builder:
		r, ok := v.(*big.Rat)
		if !ok {
			return fmt.Errorf("expected a BIGNUMERIC, got %T", v)
		}
		n, err := decimal256.FromString(r.FloatString(38), 76, 38)
		if err != nil {
			return err
		}
		bb.Append(n)
	case *array.Date32Builder:
		d, ok := v.(civil.Date)
		if !ok {
			return fmt.Errorf("expected a DATE, got %T", v)
		}
		bb.Append(arrow.Date32FromTime(d.In(time.UTC)))
	case *array.Time64Builder:
		t, ok := v.(civil.Time)
		if !ok {
			return fmt.Errorf("expected a TIME, got %T", v)
		}
		us := int64(t.Hour)*3600e6 + int64(t.Minute)*60e6 + int64(t.Second)*1e6 + int64(t.Nanosecond)/1e3
		bb.Append(arrow.Time64(us))
	case *array.TimestampBuilder:
		var t time.Time
		switch x := v.(type) {
		case time.Time:
			t = x
		case civil.DateTime:
			t = x.In(time.UTC)
		default:
			return fmt.Errorf("expected a timestamp, got %T", v)
		}
		bb.Append(arrow.Timestamp(t.UnixMicro()))
	case *array.MonthDayNanoIntervalBuilder:
		iv, ok := v.(*bigquery.IntervalValue)
		if !ok {
			return fmt.Errorf("expected an INTERVAL, got %T", v)
		}
		bb.Append(monthDayNano(iv))
	case *array.StructBuilder:
		if f.Type == bigquery.RangeFieldType {
			r, ok := v.(*bigquery.RangeValue)
			if !ok {
				return fmt.Errorf("expected a RANGE, got %T", v)
			}
			el := &bigquery.FieldSchema{Type: bigquery.DateFieldType}
			if f.RangeElementType != nil {
				el.Type = f.RangeElementType.Type
			}
			bb.Append(true)
			if err := appendValue(bb.FieldBuilder(0), el, r.Start, false); err != nil {
				return err
			}
			return appendValue(bb.FieldBuilder(1), el, r.End, false)
		}
		vals, ok := v.([]bigquery.Value)
		if !ok || len(vals) != len(f.Schema) {
			return fmt.Errorf("expected a STRUCT of %d fields, got %T", len(f.Schema), v)
		}
		bb.Append(true)
		for i, c := range f.Schema {
			if err := appendValue(bb.FieldBuilder(i), c, vals[i], true); err != nil {
				return err
			}
		}
	default:
		return fmt.Errorf("unsupported column type %s", b.Type())
	}
	return nil
}
