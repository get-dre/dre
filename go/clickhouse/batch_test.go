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
