package main

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"cloud.google.com/go/bigquery"
	"google.golang.org/api/googleapi"
	"google.golang.org/api/option"

	"github.com/get-dre/dre/go/plugin"
)

// Cancel ends a retry loop that is waiting out its backoff, without another try. No network:
// the statement is a function that always fails with a transient error.
func TestCancelEndsAWaitingRetryAtOnce(t *testing.T) {
	life, end := context.WithCancel(context.Background())
	defer end()
	s := &session{retries: 100, life: life, end: end}
	// No request is running: nothing to cancel.
	s.Cancel()

	ctx, done := s.context()
	defer done()
	tries := make(chan struct{}, 100)
	errc := make(chan error, 1)
	go func() {
		errc <- s.retry(ctx, func() error {
			tries <- struct{}{}
			return &googleapi.Error{Code: 503, Message: "backend error"}
		})
	}()
	<-tries
	time.Sleep(50 * time.Millisecond) // in the one-second wait before the second try
	start := time.Now()
	s.Cancel()
	select {
	case err := <-errc:
		if g, ok := err.(*googleapi.Error); !ok || g.Code != 503 {
			t.Fatalf("got %v", err)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("the retry loop kept waiting after Cancel")
	}
	if d := time.Since(start); d > 500*time.Millisecond {
		t.Fatalf("the retry loop took %v to stop", d)
	}
	if len(tries) != 0 {
		t.Fatalf("tried %d more times after Cancel", len(tries))
	}
	if life.Err() != nil {
		t.Fatal("Cancel ended the session's context, not the request's")
	}
}

// The next request after a cancelled one gets a live context.
func TestRequestAfterCancelIsNotCancelled(t *testing.T) {
	life, end := context.WithCancel(context.Background())
	defer end()
	s := &session{life: life, end: end}
	ctx, done := s.context()
	s.Cancel()
	if ctx.Err() == nil {
		t.Fatal("Cancel left the request's context running")
	}
	done()
	ctx, done = s.context()
	defer done()
	if ctx.Err() != nil {
		t.Fatal("the next request started cancelled")
	}
}

// fakeJobs is a BigQuery endpoint whose jobs never finish. It reports each job id it creates,
// each wait for a job, and each job it is asked to cancel. With slowInsert, jobs.insert only
// answers when its caller gives up, as when Cancel comes while BigQuery is still creating the
// job. The first readErrs waits fail with a 501, a transient error the client library doesn't
// retry by itself. With ended, BigQuery says every job has ended.
type fakeJobs struct {
	slowInsert bool
	readErrs   atomic.Int32
	ended      bool
	inserted   chan string
	reads      chan struct{}
	cancelled  chan string
}

func (f *fakeJobs) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	job := func(id, state string) {
		fmt.Fprintf(w, `{"jobReference":{"projectId":"p","jobId":%q},"status":{"state":%q},`+
			`"configuration":{"query":{"query":"select 1"}}}`, id, state)
	}
	path := strings.TrimSuffix(r.URL.Path, "/")
	switch {
	case r.Method == http.MethodPost && strings.HasSuffix(path, "/projects/p/jobs"):
		var body struct {
			JobReference struct{ JobID string } `json:"jobReference"`
		}
		_ = json.NewDecoder(r.Body).Decode(&body)
		f.inserted <- body.JobReference.JobID
		if f.slowInsert {
			<-r.Context().Done()
			return
		}
		job(body.JobReference.JobID, "RUNNING")
	case strings.HasSuffix(path, "/cancel"):
		id := path[strings.LastIndex(path, "/jobs/")+len("/jobs/") : len(path)-len("/cancel")]
		f.cancelled <- id
		fmt.Fprintf(w, `{"job":{"jobReference":{"projectId":"p","jobId":%q},"status":{"state":"DONE"}}}`, id)
	case strings.Contains(path, "/queries/"):
		f.reads <- struct{}{}
		if f.readErrs.Add(-1) >= 0 {
			w.WriteHeader(http.StatusNotImplemented)
			fmt.Fprint(w, `{"error":{"code":501,"message":"not implemented"}}`)
			return
		}
		fmt.Fprint(w, `{"jobComplete":false}`)
	case strings.Contains(path, "/jobs/"):
		state := "RUNNING"
		if f.ended {
			state = "DONE"
		}
		job(path[strings.LastIndex(path, "/")+1:], state)
	default:
		http.NotFound(w, r)
	}
}

// runAndCancel runs a statement against f with the given job_retries, and calls Cancel once f
// has seen reads waits for a job (with reads 0: once f sees the job created). It returns the
// ids of the jobs f created, and of the job f was asked to cancel.
func runAndCancel(t *testing.T, f *fakeJobs, retries, reads int) (inserted []string, cancelled string) {
	t.Helper()
	f.inserted, f.reads, f.cancelled = make(chan string, 10), make(chan struct{}, 100), make(chan string, 10)
	srv := httptest.NewServer(f)
	defer srv.Close()
	life, end := context.WithCancel(context.Background())
	defer end()
	client, err := bigquery.NewClient(life, "p", option.WithEndpoint(srv.URL), option.WithoutAuthentication())
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()
	s := &session{client: client, project: "p", retries: retries, life: life, end: end}
	errc := make(chan error, 1)
	go func() { errc <- s.Run("select 1", func(plugin.Result) error { return nil }) }()
	select {
	case id := <-f.inserted:
		inserted = append(inserted, id)
	case <-time.After(5 * time.Second):
		t.Fatal("Run didn't create a job")
	}
	for range reads {
		select {
		case <-f.reads:
		case <-time.After(5 * time.Second):
			t.Fatal("Run didn't wait for the job")
		}
	}
	time.Sleep(50 * time.Millisecond) // waiting for the job, or for BigQuery to create it
	s.Cancel()
	select {
	case err := <-errc:
		if err == nil {
			t.Fatal("a cancelled Run succeeded")
		}
	case <-time.After(5 * time.Second):
		t.Fatal("Run kept waiting after Cancel")
	}
	select {
	case cancelled = <-f.cancelled:
	case <-time.After(time.Second):
		t.Fatal("Run didn't ask BigQuery to cancel the job")
	}
	for len(f.inserted) > 0 {
		inserted = append(inserted, <-f.inserted)
	}
	return inserted, cancelled
}

// A cancelled Run asks BigQuery to cancel its job, not only to stop waiting for it.
func TestCancelCancelsTheRunningJob(t *testing.T) {
	inserted, cancelled := runAndCancel(t, &fakeJobs{}, 0, 0)
	if len(inserted) != 1 || cancelled != inserted[0] {
		t.Fatalf("cancelled job %q, ran %q", cancelled, inserted)
	}
}

// A Cancel that comes while BigQuery is still creating the job cancels that job too.
func TestCancelDuringJobCreationCancelsTheJob(t *testing.T) {
	inserted, cancelled := runAndCancel(t, &fakeJobs{slowInsert: true}, 0, 0)
	if len(inserted) != 1 || inserted[0] == "" || cancelled != inserted[0] {
		t.Fatalf("cancelled job %q, ran %q", cancelled, inserted)
	}
}

// A transient error while waiting for a running job retries the wait, not the statement: a
// second job would run and bill too, and Cancel would stop only the second.
func TestRetryWaitsForTheSameJob(t *testing.T) {
	f := &fakeJobs{}
	f.readErrs.Store(1)
	inserted, cancelled := runAndCancel(t, f, 3, 2)
	if len(inserted) != 1 || cancelled != inserted[0] {
		t.Fatalf("ran jobs %q, cancelled %q; want one job, cancelled", inserted, cancelled)
	}
}

// Once BigQuery says the failed job has ended, the retry runs the statement as a new job.
func TestRetryRunsANewJobOnceTheFirstEnded(t *testing.T) {
	f := &fakeJobs{ended: true}
	f.readErrs.Store(1)
	inserted, cancelled := runAndCancel(t, f, 3, 2)
	if len(inserted) != 2 || cancelled != inserted[1] {
		t.Fatalf("ran jobs %q, cancelled %q; want two jobs, the second cancelled", inserted, cancelled)
	}
}

// When the retries give up while the job still runs, Run cancels the job instead of leaving it
// to run and bill with nobody reading it.
func TestGivingUpCancelsTheRunningJob(t *testing.T) {
	f := &fakeJobs{}
	f.readErrs.Store(100)
	f.inserted, f.reads, f.cancelled = make(chan string, 10), make(chan struct{}, 100), make(chan string, 10)
	srv := httptest.NewServer(f)
	defer srv.Close()
	life, end := context.WithCancel(context.Background())
	defer end()
	client, err := bigquery.NewClient(life, "p", option.WithEndpoint(srv.URL), option.WithoutAuthentication())
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()
	s := &session{client: client, project: "p", retries: 1, life: life, end: end}
	if err := s.Run("select 1", func(plugin.Result) error { return nil }); err == nil {
		t.Fatal("Run succeeded though every wait failed")
	}
	inserted := <-f.inserted
	select {
	case cancelled := <-f.cancelled:
		if cancelled != inserted {
			t.Fatalf("cancelled job %q, ran %q", cancelled, inserted)
		}
	default:
		t.Fatal("Run gave up without cancelling the running job")
	}
}
