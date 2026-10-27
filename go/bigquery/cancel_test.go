package main

import (
	"context"
	"testing"
	"time"

	"google.golang.org/api/googleapi"
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
