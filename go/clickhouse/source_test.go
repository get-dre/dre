package main

import (
	"context"
	"errors"
	"sync"
	"testing"

	"github.com/get-dre/dre/go/plugin"
	"github.com/get-dre/dre/go/plugin/plugintest"
)

// Pause after the SDK accepts execute, before ClickHouse creates its query context.
type delayedStart struct {
	session
	entered   chan struct{}
	cancelled chan struct{}
	observed  chan error
	once      sync.Once
}

func (s *delayedStart) Run(_ string, _ func(plugin.Result) error) error {
	close(s.entered)
	<-s.cancelled
	ctx, done := s.start()
	defer done()
	s.observed <- ctx.Err()
	return errors.New("query stopped")
}

func (s *delayedStart) Cancel() {
	s.session.Cancel()
	s.once.Do(func() { close(s.cancelled) })
}

func (s *delayedStart) Close() {}

func TestCancelBeforeQueryContextExists(t *testing.T) {
	s := &delayedStart{
		entered: make(chan struct{}), cancelled: make(chan struct{}), observed: make(chan error, 1),
	}
	role := sourceRole
	role.Open = func(map[string]any) (plugin.Session, error) { return s, nil }
	c := plugintest.Start(t, pkg, role)
	c.Hello()
	c.Open(map[string]any{})
	c.Send(map[string]any{"type": "execute", "id": 2, "sql": "SELECT count() FROM system.numbers"})
	<-s.entered
	c.Send(map[string]any{"type": "cancel", "id": 2})
	if r := c.Reply(); r["type"] != "error" || r["kind"] != "cancelled" {
		t.Fatalf("cancel reply: %v", r)
	}
	if err := <-s.observed; !errors.Is(err, context.Canceled) {
		t.Fatalf("query context after cancel: %v; want context.Canceled", err)
	}
}
