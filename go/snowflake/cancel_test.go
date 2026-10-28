package main

import (
	"context"
	"testing"
)

// Cancel ends the running request's context, which the driver watches to abort the query in
// Snowflake. The session's own context, and the next request's, stay live. No network.
func TestCancelEndsTheRunningRequestOnly(t *testing.T) {
	life, end := context.WithCancel(context.Background())
	defer end()
	s := &session{life: life, end: end}
	// No request is running: nothing to cancel.
	s.Cancel()

	ctx, done := s.Start(s.life)
	q := queryContext(ctx)
	s.Cancel()
	if q.Err() == nil {
		t.Fatal("Cancel left the query's context running")
	}
	if life.Err() != nil {
		t.Fatal("Cancel ended the session's context, not the request's")
	}
	done()
	ctx, done = s.Start(s.life)
	defer done()
	if ctx.Err() != nil {
		t.Fatal("the next request started cancelled")
	}
}
