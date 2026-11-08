package main

import (
	"io"
	"strings"
	"testing"

	"github.com/ClickHouse/clickhouse-go/v2/lib/driver"
	"github.com/apache/arrow-go/v18/arrow"
)

// wideRows is n rows of one String column, each value size bytes long.
type wideRows struct {
	driver.Rows
	n, size int
}

func (w *wideRows) Next() bool { w.n--; return w.n >= 0 }
func (w *wideRows) Err() error { return nil }
func (w *wideRows) Scan(dest ...any) error {
	*dest[0].(*string) = strings.Repeat("x", w.size)
	return nil
}

func TestWideRowsEndABatchAtItsByteBudget(t *testing.T) {
	defer func(old int) { batchBytes = old }(batchBytes)
	batchBytes = 2500
	col := newColumn("body", "String")
	r := &result{rows: &wideRows{n: 10, size: 1000}, cols: []column{col}, dest: []any{new(string)},
		schema: arrow.NewSchema([]arrow.Field{{Name: "body", Type: col.arrow, Nullable: true}}, nil)}
	var sizes []int64
	for {
		rec, err := r.Next()
		if err == io.EOF {
			break
		}
		if err != nil {
			t.Fatal(err)
		}
		sizes = append(sizes, rec.NumRows())
		rec.Release()
	}
	if len(sizes) != 4 || sizes[0] != 3 || sizes[3] != 1 {
		t.Fatal(sizes)
	}
}

// arrayRows is n rows of one Array(String) column, each array holding elems values of size bytes.
type arrayRows struct {
	driver.Rows
	n, elems, size int
}

func (a *arrayRows) Next() bool { a.n--; return a.n >= 0 }
func (a *arrayRows) Err() error { return nil }
func (a *arrayRows) Scan(dest ...any) error {
	v := make([]string, a.elems)
	for i := range v {
		v[i] = strings.Repeat("x", a.size)
	}
	*dest[0].(*[]string) = v
	return nil
}

func batchSizes(t *testing.T, r *result) ([]int64, error) {
	t.Helper()
	var sizes []int64
	for {
		rec, err := r.Next()
		if err == io.EOF {
			return sizes, nil
		}
		if err != nil {
			return sizes, err
		}
		sizes = append(sizes, rec.NumRows())
		rec.Release()
	}
}

func TestNestedValuesAreChargedTheirJSONText(t *testing.T) {
	defer func(old int) { batchBytes = old }(batchBytes)
	batchBytes = 2500
	col := newColumn("tags", "Array(String)")
	r := &result{rows: &arrayRows{n: 10, elems: 10, size: 100}, cols: []column{col}, dest: []any{new([]string)},
		schema: arrow.NewSchema([]arrow.Field{{Name: "tags", Type: col.arrow, Nullable: true}}, nil)}
	sizes, err := batchSizes(t, r)
	if err != nil {
		t.Fatal(err)
	}
	// Each row is about 1,000 bytes of JSON text: three rows fill a 2,500-byte budget.
	if len(sizes) != 4 || sizes[0] != 3 {
		t.Fatal(sizes)
	}
}

func TestAnOversizedCellIsAnError(t *testing.T) {
	defer func(old int) { cellBytes = old }(cellBytes)
	cellBytes = 500
	col := newColumn("body", "String")
	r := &result{rows: &wideRows{n: 2, size: 1000}, cols: []column{col}, dest: []any{new(string)},
		schema: arrow.NewSchema([]arrow.Field{{Name: "body", Type: col.arrow, Nullable: true}}, nil)}
	if _, err := batchSizes(t, r); err == nil || !strings.Contains(err.Error(), "body") {
		t.Fatal(err)
	}
}
