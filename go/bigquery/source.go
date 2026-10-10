package main

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"math"
	"strconv"
	"strings"
	"time"

	"cloud.google.com/go/bigquery"
	"github.com/apache/arrow-go/v18/arrow"
	"github.com/apache/arrow-go/v18/arrow/array"
	"google.golang.org/api/googleapi"
	"google.golang.org/api/option"

	"github.com/get-dre/dre/go/plugin"
)

// session is one BigQuery session: every job carries its session_id, so temp tables and
// session variables from one statement are there for the next.
type session struct {
	client    *bigquery.Client
	project   string // where tables are read from by default
	dataset   string
	id        string // the BigQuery session; "" when the server doesn't do sessions (emulator)
	priority  bigquery.QueryPriority
	maxBilled int64
	jobTime   time.Duration // job_execution_timeout_seconds
	startTime time.Duration // job_creation_timeout_seconds
	retries   int
	retryFor  time.Duration
}

func open(conn map[string]any) (*session, error) {
	project := first(conn, "project", "database")
	if project == "" {
		return nil, fmt.Errorf("the profile output needs a `project` field (dbt's `database`)")
	}
	s := &session{project: project, dataset: first(conn, "dataset", "schema")}
	var err error
	if s.maxBilled, err = plugin.Int(conn, "maximum_bytes_billed", 0); err != nil {
		return nil, err
	}
	secs := func(keys ...string) (time.Duration, error) {
		for _, k := range keys {
			n, err := plugin.Int(conn, k, 0)
			if err != nil || n > 0 {
				return time.Duration(n) * time.Second, err
			}
		}
		return 0, nil
	}
	if s.jobTime, err = secs("job_execution_timeout_seconds", "timeout_seconds"); err != nil {
		return nil, err
	}
	if s.startTime, err = secs("job_creation_timeout_seconds"); err != nil {
		return nil, err
	}
	if s.retryFor, err = secs("job_retry_deadline_seconds"); err != nil {
		return nil, err
	}
	retries, err := plugin.Int(conn, "job_retries", 1)
	if err != nil {
		return nil, err
	}
	s.retries = int(retries)
	switch p := strings.ToLower(plugin.Optional(conn, "priority")); p {
	case "", "interactive":
		s.priority = bigquery.InteractivePriority
	case "batch":
		s.priority = bigquery.BatchPriority
	default:
		return nil, fmt.Errorf("`priority` must be interactive or batch, got `%s`", p)
	}

	ctx := context.Background()
	ts, err := tokenSource(ctx, conn)
	if err != nil {
		return nil, err
	}
	opts := []option.ClientOption{option.WithTokenSource(ts), option.WithUserAgent("dre/" + version)}
	endpoint := plugin.Optional(conn, "api_endpoint")
	if endpoint != "" {
		opts = append(opts, option.WithEndpoint(endpoint))
	}
	if q := plugin.Optional(conn, "quota_project"); q != "" {
		opts = append(opts, option.WithQuotaProject(q))
	}
	execProject := plugin.Optional(conn, "execution_project")
	if execProject == "" {
		execProject = project
	}
	s.client, err = bigquery.NewClient(ctx, execProject, opts...)
	if err != nil {
		return nil, fmt.Errorf("can't start the BigQuery client: %v", err)
	}
	s.client.Location = plugin.Optional(conn, "location")
	// The Storage Read API serves large results as Arrow; small ones and identities without
	// bigquery.readsessions.create stay on the REST API (the client falls back by itself). A
	// custom endpoint has no known Storage API address, so it reads over REST only.
	if endpoint == "" {
		if err := s.client.EnableStorageReadClient(ctx, opts...); err != nil {
			debugf("the Storage Read API isn't available (%v); results are read over the REST API", err)
		}
	}
	// Tried again (`retries`) while starting the session, before any query of the report.
	rules, _, err := plugin.RulesFrom(plugin.DefaultRules(), conn, nil, nil)
	if err != nil {
		s.client.Close()
		return nil, err
	}
	_, err = plugin.Retry(rules.Retries, "starting the BigQuery session", func() (struct{}, error) {
		err := s.startSession()
		if err != nil && (transient(err) || plugin.IsConnectionError(err)) {
			return struct{}{}, &plugin.TemporaryError{Err: err}
		}
		return struct{}{}, err
	})
	if err != nil {
		s.client.Close()
		return nil, err
	}
	return s, nil
}

// startSession creates the BigQuery session every later job joins.
func (s *session) startSession() error {
	q := s.query("SELECT 1")
	q.CreateSession = true
	ctx, cancel := s.context()
	defer cancel()
	job, err := q.Run(ctx)
	if err != nil {
		return openErr(err)
	}
	st, err := job.Wait(ctx)
	if err == nil {
		err = st.Err()
	}
	if err != nil {
		return openErr(err)
	}
	if st.Statistics != nil && st.Statistics.SessionInfo != nil {
		s.id = st.Statistics.SessionInfo.SessionID
	}
	if s.id == "" {
		debugf("the server didn't start a session; temp tables may not last between statements")
	}
	return nil
}

func (s *session) query(sql string) *bigquery.Query {
	q := s.client.Query(sql)
	q.DefaultProjectID = s.project
	q.DefaultDatasetID = s.dataset
	q.Priority = s.priority
	q.MaxBytesBilled = s.maxBilled
	q.JobTimeout = s.jobTime
	if s.id != "" {
		q.ConnectionProperties = []*bigquery.ConnectionProperty{{Key: "session_id", Value: s.id}}
	}
	return q
}

// context bounds one job: creation plus execution timeouts, when set.
func (s *session) context() (context.Context, context.CancelFunc) {
	if s.jobTime > 0 || s.startTime > 0 {
		return context.WithTimeout(context.Background(), s.jobTime+s.startTime+time.Minute)
	}
	return context.WithCancel(context.Background())
}

// Run executes one statement. A statement that fails with a transient error runs again, up to
// job_retries times within job_retry_deadline_seconds.
func (s *session) Run(sql string, fn func(plugin.Result) error) error {
	ctx, cancel := s.context()
	defer cancel()
	start := time.Now()
	var it *bigquery.RowIterator
	var err error
	for attempt := 0; ; attempt++ {
		it, err = s.query(sql).Read(ctx)
		if err == nil || attempt >= s.retries || !transient(err) || (s.retryFor > 0 && time.Since(start) > s.retryFor) {
			break
		}
		debugf("retrying after a transient error: %v", err)
		time.Sleep(time.Duration(attempt+1) * time.Second)
	}
	if err != nil {
		return cleanErr(err)
	}
	res, err := newResult(it)
	if err == errNoResult {
		return fn(nil)
	}
	if err != nil {
		return cleanErr(err)
	}
	return fn(res)
}

// Check dry-runs the statement: BigQuery validates it and reports the bytes it would scan.
func (s *session) Check(sql string) (string, error) {
	q := s.query(sql)
	q.DryRun = true
	ctx, cancel := s.context()
	defer cancel()
	job, err := q.Run(ctx)
	if err != nil {
		return "", cleanErr(err)
	}
	st := job.LastStatus()
	if st == nil || st.Statistics == nil {
		return "", nil
	}
	return fmt.Sprintf("dry run: would process %s", bytesText(st.Statistics.TotalBytesProcessed)), nil
}

// Load puts the rows into a session temp table, as one CREATE TEMP TABLE ... AS SELECT over an
// array of structs: a session has no bulk path into a temp table.
func (s *session) Load(name string, schema *arrow.Schema, recs []arrow.Record) (plugin.Loaded, error) {
	table := "dre_lookup_" + name
	sql, rows, err := tempTableSQL(table, schema, recs)
	if err != nil {
		return plugin.Loaded{}, err
	}
	if err := s.Run(sql, func(plugin.Result) error { return nil }); err != nil {
		return plugin.Loaded{}, err
	}
	return plugin.Loaded{
		Relation: table, Rows: rows,
		Warning: fmt.Sprintf("BigQuery has no bulk load into a session's temp table, so %d rows were sent as one SQL statement. Data this size probably belongs in a table in BigQuery", rows),
	}, nil
}

func (s *session) Close() {
	if s.id != "" {
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
		if job, err := s.query("CALL BQ.ABORT_SESSION()").Run(ctx); err == nil {
			_, _ = job.Wait(ctx)
		}
		cancel()
	}
	s.client.Close()
}

// tempTableSQL is a temp table holding recs. Every column is typed in the array's STRUCT, so an
// empty or all-NULL column still gets its type.
func tempTableSQL(table string, schema *arrow.Schema, recs []arrow.Record) (string, int64, error) {
	var cols []string
	for _, f := range schema.Fields() {
		var t string
		switch f.Type.ID() {
		case arrow.STRING:
			t = "STRING"
		case arrow.INT64:
			t = "INT64"
		case arrow.FLOAT64:
			t = "FLOAT64"
		case arrow.BOOL:
			t = "BOOL"
		case arrow.DATE32:
			t = "DATE"
		default:
			return "", 0, fmt.Errorf("can't load a column of type %s", f.Type)
		}
		cols = append(cols, quote(f.Name)+" "+t)
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
	return fmt.Sprintf("CREATE OR REPLACE TEMP TABLE %s AS SELECT * FROM UNNEST(ARRAY<STRUCT<%s>>[\n  %s\n])",
		quote(table), strings.Join(cols, ", "), strings.Join(rows, ",\n  ")), n, nil
}

func quote(ident string) string {
	return "`" + strings.ReplaceAll(ident, "`", "\\`") + "`"
}

// literal is one cell as a GoogleSQL literal.
func literal(col arrow.Array, i int) string {
	if col.IsNull(i) {
		return "NULL"
	}
	switch c := col.(type) {
	case *array.String:
		return strconv.Quote(c.Value(i)) // GoogleSQL takes the same escapes as Go in "..."
	case *array.Int64:
		return strconv.FormatInt(c.Value(i), 10)
	case *array.Float64:
		v := c.Value(i)
		switch {
		case math.IsNaN(v):
			return "CAST('NaN' AS FLOAT64)"
		case math.IsInf(v, 1):
			return "CAST('inf' AS FLOAT64)"
		case math.IsInf(v, -1):
			return "CAST('-inf' AS FLOAT64)"
		}
		return "CAST(" + strconv.FormatFloat(v, 'g', -1, 64) + " AS FLOAT64)"
	case *array.Boolean:
		return strconv.FormatBool(c.Value(i))
	case *array.Date32:
		return "DATE '" + c.Value(i).ToTime().Format("2006-01-02") + "'"
	}
	return "NULL"
}

// transient is true for errors that may pass on a second try: server errors and rate limits.
func transient(err error) bool {
	var g *googleapi.Error
	if errors.As(err, &g) {
		if g.Code >= 500 {
			return true
		}
		for _, e := range g.Errors {
			if e.Reason == "rateLimitExceeded" {
				return true
			}
		}
	}
	return false
}

// cleanErr keeps BigQuery's message, without the API's request details.
func cleanErr(err error) error {
	if err == nil {
		return nil
	}
	var g *googleapi.Error
	if errors.As(err, &g) && g.Message != "" {
		return errors.New(g.Message)
	}
	return err
}

func openErr(err error) error {
	err = cleanErr(err)
	msg := err.Error()
	hint := ""
	switch {
	case strings.Contains(msg, "401") || strings.Contains(strings.ToLower(msg), "invalid_grant") || strings.Contains(msg, "Request had invalid authentication"):
		hint = " (check the sign-in; for method oauth, `gcloud auth application-default login`)"
	case strings.Contains(msg, "403") || strings.Contains(msg, "Access Denied") || strings.Contains(msg, "permission"):
		hint = " (the signed-in identity needs bigquery.jobs.create on the execution project)"
	case strings.Contains(msg, "404") || strings.Contains(msg, "Not found"):
		hint = " (check `project` and `location`)"
	}
	return fmt.Errorf("can't open a BigQuery session%s: %s", hint, msg)
}

func bytesText(n int64) string {
	units := []string{"bytes", "KB", "MB", "GB", "TB", "PB"}
	f, u := float64(n), 0
	for f >= 1024 && u < len(units)-1 {
		f /= 1024
		u++
	}
	if u == 0 {
		return fmt.Sprintf("%d bytes", n)
	}
	return fmt.Sprintf("%.1f %s", f, units[u])
}

func first(conn map[string]any, keys ...string) string {
	for _, k := range keys {
		if v := plugin.Optional(conn, k); v != "" {
			return v
		}
	}
	return ""
}

func debugf(format string, args ...any) {
	slog.Debug(fmt.Sprintf(format, args...))
}
