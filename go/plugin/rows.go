package plugin

import (
	"strings"

	"github.com/apache/arrow-go/v18/arrow"
)

// SQLRows renders records in order as parenthesized SQL rows. The caller supplies
// its warehouse's literal rules and retains ownership of the records.
func SQLRows(recs []arrow.RecordBatch, literal func(arrow.Array, int) string) ([]string, int64) {
	var rows []string
	var n int64
	for _, rec := range recs {
		for r := 0; r < int(rec.NumRows()); r++ {
			vals := make([]string, rec.NumCols())
			for c := range vals {
				vals[c] = literal(rec.Column(c), r)
			}
			rows = append(rows, "("+strings.Join(vals, ", ")+")")
			n++
		}
	}
	return rows, n
}
