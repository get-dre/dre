package main

import (
	"context"
	"database/sql"
	"database/sql/driver"
	"errors"
	"fmt"
	"io"
	"math"
	"os"
	"strconv"
	"strings"

	"github.com/apache/arrow-go/v18/arrow"
	"github.com/apache/arrow-go/v18/arrow/array"
	sf "github.com/snowflakedb/gosnowflake/v2"
	"github.com/snowflakedb/gosnowflake/v2/arrowbatches"

	"github.com/get-dre/dre/go/plugin"
)

// session holds one connection from database/sql, which is one Snowflake session: temp tables
// and session settings from one statement are there for the next.
type session struct {
	db   *sql.DB
	conn *sql.Conn
}

func open(conn map[string]any) (*session, error) {
	cfg, err := config(conn)
	if err != nil {
		return nil, err
	}
	if os.Getenv("SNOWFLAKE_LOG_LEVEL") == "" {
		_ = sf.GetLogger().SetLogLevel("OFF")
	}
	db := sql.OpenDB(sf.NewConnector(sf.SnowflakeDriver{}, *cfg))
	db.SetMaxOpenConns(1)
	c, err := db.Conn(context.Background())
	if err == nil {
		// Sign in now, so a bad credential fails `open`, not the first statement.
		err = c.PingContext(context.Background())
	}
	if err != nil {
		db.Close()
		return nil, openErr(err, cfg)
	}
	return &session{db: db, conn: c}, nil
}

// queryContext asks for Arrow batches, NUMBER kept exact, timestamps in microseconds.
func queryContext() context.Context {
	ctx := sf.WithHigherPrecision(context.Background())
	ctx = arrowbatches.WithArrowBatches(ctx)
	return arrowbatches.WithTimestampOption(ctx, arrowbatches.UseMicrosecondTimestamp)
}

func (s *session) Run(query string, fn func(plugin.Result) error) error {
	ctx := queryContext()
	err := s.conn.Raw(func(dc any) error {
		q, ok := dc.(driver.QueryerContext)
		if !ok {
			return fmt.Errorf("the Snowflake connection can't run queries")
		}
		rows, err := q.QueryContext(ctx, query, nil)
		if err != nil {
			return err
		}
		defer rows.Close()
		names := rows.Columns()
		if len(names) == 0 {
			return fn(nil)
		}
		cols := make([]column, len(names))
		for i, n := range names {
			cols[i] = column{name: n}
			if t, ok := rows.(driver.RowsColumnTypeDatabaseTypeName); ok {
				cols[i].typ = t.ColumnTypeDatabaseTypeName(i)
			}
			if t, ok := rows.(driver.RowsColumnTypePrecisionScale); ok {
				cols[i].precision, cols[i].scale, _ = t.ColumnTypePrecisionScale(i)
			}
		}
		sr, ok := rows.(sf.SnowflakeRows)
		if !ok {
			return fmt.Errorf("the Snowflake driver didn't return Arrow results")
		}
		batches, err := arrowbatches.GetArrowBatches(sr)
		if err != nil {
			return err
		}
		return fn(newResult(cols, batches))
	})
	return cleanErr(err)
}

// Check runs EXPLAIN, which compiles the statement without running it.
func (s *session) Check(query string) (string, error) {
	rows, err := s.conn.QueryContext(context.Background(), "EXPLAIN "+query)
	if err != nil {
		return "", cleanErr(err)
	}
	defer rows.Close()
	for rows.Next() {
	}
	return "", cleanErr(rows.Err())
}

// Load puts the rows into a temporary table, as one statement over VALUES.
func (s *session) Load(name string, schema *arrow.Schema, recs []arrow.Record) (plugin.Loaded, error) {
	table := "dre_lookup_" + name
	sql, rows, err := tempTableSQL(table, schema, recs)
	if err != nil {
		return plugin.Loaded{}, err
	}
	if _, err := s.conn.ExecContext(context.Background(), sql); err != nil {
		return plugin.Loaded{}, cleanErr(err)
	}
	return plugin.Loaded{
		Relation: quote(table), Rows: rows,
		Warning: fmt.Sprintf("%d rows were sent to Snowflake as one SQL statement into a temporary table. Data this size probably belongs in a table in Snowflake", rows),
	}, nil
}

func (s *session) Close() {
	s.conn.Close()
	s.db.Close()
}

// result is one statement's batches, each normalised against the column metadata.
type result struct {
	cols    []column
	schema  *arrow.Schema
	batches []*arrowbatches.ArrowBatch
	pending []arrow.Record
}

func newResult(cols []column, batches []*arrowbatches.ArrowBatch) *result {
	return &result{cols: cols, schema: declaredSchema(cols), batches: batches}
}

func (r *result) Schema() (*arrow.Schema, error) { return r.schema, nil }

func (r *result) HasNext() bool { return len(r.pending) > 0 || len(r.batches) > 0 }

func (r *result) Next() (arrow.Record, error) {
	for len(r.pending) == 0 {
		if len(r.batches) == 0 {
			return nil, io.EOF
		}
		recs, err := r.batches[0].Fetch()
		r.batches = r.batches[1:]
		if err != nil {
			return nil, cleanErr(err)
		}
		r.pending = append(r.pending, *recs...)
	}
	rec := r.pending[0]
	r.pending = r.pending[1:]
	defer rec.Release()
	return normalise(rec, r.cols)
}

// tempTableSQL is a temporary table holding recs. Every value is sent as text and cast to its
// column's type, so NULLs and NaN need no special VALUES typing.
func tempTableSQL(table string, schema *arrow.Schema, recs []arrow.Record) (string, int64, error) {
	var defs, sel []string
	for i, f := range schema.Fields() {
		var t string
		switch f.Type.ID() {
		case arrow.STRING:
			t = "VARCHAR"
		case arrow.INT64:
			t = "NUMBER(38,0)"
		case arrow.FLOAT64:
			t = "FLOAT"
		case arrow.BOOL:
			t = "BOOLEAN"
		case arrow.DATE32:
			t = "DATE"
		default:
			return "", 0, fmt.Errorf("can't load a column of type %s", f.Type)
		}
		defs = append(defs, quote(f.Name)+" "+t)
		sel = append(sel, fmt.Sprintf("$%d::%s AS %s", i+1, t, quote(f.Name)))
	}
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
	if n == 0 {
		return fmt.Sprintf("CREATE OR REPLACE TEMPORARY TABLE %s (%s)", quote(table), strings.Join(defs, ", ")), 0, nil
	}
	return fmt.Sprintf("CREATE OR REPLACE TEMPORARY TABLE %s AS SELECT %s FROM VALUES\n  %s",
		quote(table), strings.Join(sel, ", "), strings.Join(rows, ",\n  ")), n, nil
}

func quote(ident string) string {
	return `"` + strings.ReplaceAll(ident, `"`, `""`) + `"`
}

// literal is one cell as a Snowflake string literal (cast by tempTableSQL), or NULL.
func literal(col arrow.Array, i int) string {
	if col.IsNull(i) {
		return "NULL"
	}
	var v string
	switch c := col.(type) {
	case *array.String:
		v = c.Value(i)
	case *array.Int64:
		v = strconv.FormatInt(c.Value(i), 10)
	case *array.Float64:
		f := c.Value(i)
		switch {
		case math.IsNaN(f):
			v = "NaN"
		case math.IsInf(f, 1):
			v = "inf"
		case math.IsInf(f, -1):
			v = "-inf"
		default:
			v = strconv.FormatFloat(f, 'g', -1, 64)
		}
	case *array.Boolean:
		v = strings.ToUpper(strconv.FormatBool(c.Value(i)))
	case *array.Date32:
		v = c.Value(i).ToTime().Format("2006-01-02")
	default:
		return "NULL"
	}
	// Snowflake reads backslash escapes inside '...'.
	v = strings.ReplaceAll(v, `\`, `\\`)
	return "'" + strings.ReplaceAll(v, "'", `\'`) + "'"
}

// cleanErr keeps the driver's message without its query ID boilerplate.
func cleanErr(err error) error {
	if err == nil {
		return nil
	}
	var se *sf.SnowflakeError
	if errors.As(err, &se) && se.Message != "" {
		msg := se.Message
		if se.Number != 0 {
			msg = fmt.Sprintf("%s (Snowflake error %d)", msg, se.Number)
		}
		return errors.New(msg)
	}
	return err
}

func openErr(err error, cfg *sf.Config) error {
	err = cleanErr(err)
	msg := err.Error()
	hint := ""
	lower := strings.ToLower(msg)
	switch {
	case strings.Contains(lower, "incorrect username or password") || strings.Contains(lower, "jwt token is invalid") || strings.Contains(lower, "invalid oauth"):
		hint = " (check `user` and the sign-in fields)"
	case strings.Contains(lower, "no such host") || strings.Contains(lower, "404"):
		hint = fmt.Sprintf(" (check `account`: %q)", cfg.Account)
	}
	return fmt.Errorf("can't open a Snowflake session%s: %s", hint, msg)
}
