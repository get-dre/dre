package plugin

import (
	"reflect"
	"strconv"
	"testing"

	"github.com/apache/arrow-go/v18/arrow"
	"github.com/apache/arrow-go/v18/arrow/array"
	"github.com/apache/arrow-go/v18/arrow/memory"
)

func TestSQLRows(t *testing.T) {
	schema := arrow.NewSchema([]arrow.Field{{Name: "n", Type: arrow.PrimitiveTypes.Int64, Nullable: true}, {Name: "other", Type: arrow.PrimitiveTypes.Int64}}, nil)
	record := func(values []int64, valid []bool) arrow.RecordBatch {
		builder := array.NewInt64Builder(memory.DefaultAllocator)
		defer builder.Release()
		builder.AppendValues(values, valid)
		col := builder.NewArray()
		defer col.Release()
		builder.AppendValues(values, nil)
		other := builder.NewArray()
		defer other.Release()
		rec := array.NewRecordBatch(schema, []arrow.Array{col, other}, int64(len(values)))
		t.Cleanup(rec.Release)
		return rec
	}
	first := record([]int64{1, 0}, []bool{true, false})
	second := record([]int64{2}, nil)
	empty := record(nil, nil)
	for _, tt := range []struct {
		name  string
		recs  []arrow.RecordBatch
		want  []string
		count int64
	}{
		{"multiple records and nulls", []arrow.RecordBatch{first, second}, []string{"(1, 1)", "(NULL, 0)", "(2, 2)"}, 3},
		{"no records", nil, nil, 0},
		{"zero rows", []arrow.RecordBatch{empty}, nil, 0},
	} {
		t.Run(tt.name, func(t *testing.T) {
			rows, count := SQLRows(tt.recs, func(col arrow.Array, row int) string {
				if col.IsNull(row) {
					return "NULL"
				}
				return strconv.FormatInt(col.(*array.Int64).Value(row), 10)
			})
			if !reflect.DeepEqual(rows, tt.want) || count != tt.count {
				t.Fatalf("SQLRows = %v, %d; want %v, %d", rows, count, tt.want, tt.count)
			}
		})
	}
}
