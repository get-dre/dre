package main

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
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

// fakeJobs is a BigQuery endpoint whose jobs never finish. It reports each job id it creates
// and each it is asked to cancel. With slowInsert, jobs.insert only answers when its caller
// gives up, as when Cancel comes while BigQuery is still creating the job.
type fakeJobs struct {
	slowInsert bool
	inserted   chan string
	cancelled  chan string
}

func (f *fakeJobs) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	running := func(id string) {
		fmt.Fprintf(w, `{"jobReference":{"projectId":"p","jobId":%q},"status":{"state":"RUNNING"},`+
			`"configuration":{"query":{"query":"select 1"}}}`, id)
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
		running(body.JobReference.JobID)
	case strings.HasSuffix(path, "/cancel"):
		id := path[strings.LastIndex(path, "/jobs/")+len("/jobs/") : len(path)-len("/cancel")]
		f.cancelled <- id
		fmt.Fprintf(w, `{"job":{"jobReference":{"projectId":"p","jobId":%q},"status":{"state":"DONE"}}}`, id)
	case strings.Contains(path, "/queries/"):
		fmt.Fprint(w, `{"jobComplete":false}`)
	case strings.Contains(path, "/jobs/"):
		running(path[strings.LastIndex(path, "/")+1:])
	default:
		http.NotFound(w, r)
	}
}

// runAndCancel runs a statement against f, calls Cancel when f sees the job created, and
// returns the job id f was asked to cancel.
func runAndCancel(t *testing.T, f *fakeJobs) (inserted, cancelled string) {
	t.Helper()
	f.inserted, f.cancelled = make(chan string, 10), make(chan string, 10)
	srv := httptest.NewServer(f)
	defer srv.Close()
	life, end := context.WithCancel(context.Background())
	defer end()
	client, err := bigquery.NewClient(life, "p", option.WithEndpoint(srv.URL), option.WithoutAuthentication())
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()
	s := &session{client: client, project: "p", life: life, end: end}
	errc := make(chan error, 1)
	go func() { errc <- s.Run("select 1", func(plugin.Result) error { return nil }) }()
	select {
	case inserted = <-f.inserted:
	case <-time.After(5 * time.Second):
		t.Fatal("Run didn't create a job")
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
	return inserted, cancelled
}

// A cancelled Run asks BigQuery to cancel its job, not only to stop waiting for it.
func TestCancelCancelsTheRunningJob(t *testing.T) {
	inserted, cancelled := runAndCancel(t, &fakeJobs{})
	if cancelled != inserted {
		t.Fatalf("cancelled job %q, ran %q", cancelled, inserted)
	}
}

// A Cancel that comes while BigQuery is still creating the job cancels that job too.
func TestCancelDuringJobCreationCancelsTheJob(t *testing.T) {
	inserted, cancelled := runAndCancel(t, &fakeJobs{slowInsert: true})
	if inserted == "" || cancelled != inserted {
		t.Fatalf("cancelled job %q, ran %q", cancelled, inserted)
	}
}
