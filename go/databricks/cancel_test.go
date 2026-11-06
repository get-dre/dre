package main

// Timeouts, cancel and refused paths and hosts, against fake servers and a fake SQL driver.

import (
	"context"
	"database/sql"
	"database/sql/driver"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/get-dre/dre/go/plugin"
	"github.com/get-dre/dre/go/plugin/plugintest"
)

// stalled is a Files API that takes each request and never answers until the test ends; each
// request is announced on arrived.
func stalled(t *testing.T) (*httptest.Server, chan struct{}) {
	arrived, release := make(chan struct{}, 8), make(chan struct{})
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		arrived <- struct{}{}
		select {
		case <-release:
		case <-r.Context().Done():
		}
	}))
	t.Cleanup(func() { close(release); srv.Close() })
	return srv, arrived
}

func TestAStalledUploadTimesOut(t *testing.T) {
	srv, _ := stalled(t)
	_, err := deliverToVolume(localFile(t, "x"), "/Volumes/c/s/v/x.csv",
		map[string]any{"host": srv.URL, "token": "good", "timeout": "1s", "retries": 0.0}, nil)
	if err == nil || !strings.Contains(err.Error(), "timeout") {
		t.Fatal(err)
	}
}

func TestCancelStopsAnUpload(t *testing.T) {
	srv, arrived := stalled(t)
	c := plugintest.Start(t, pkg, destinationRole)
	c.Hello()
	c.Send(map[string]any{"type": "deliver", "id": 7, "local_path": localFile(t, "x"), "remote_path": "/Workspace/Shared/x.csv",
		"connection": map[string]any{"host": srv.URL, "token": "good"}, "options": map[string]any{}})
	<-arrived
	c.Send(map[string]any{"type": "cancel", "id": 7})
	if r := c.Reply(); r["type"] != "error" || r["kind"] != "cancelled" {
		t.Fatalf("%v", r)
	}
}

func TestVolumePathsAndPlainHTTPHostsAreRefused(t *testing.T) {
	for _, p := range []string{"/Volumes/c/s/v/../../../../tmp/x", "/Volumes/c/s/v/./x"} {
		if _, err := deliverToVolume(localFile(t, "x"), p, map[string]any{"host": "h", "token": "t"}, nil); err == nil ||
			!strings.Contains(err.Error(), "must be /Volumes/") {
			t.Fatal(p, err)
		}
	}
	for _, host := range []string{"http://adb-1.azuredatabricks.net", "http://10.0.0.1:8080"} {
		_, err := deliverToVolume(localFile(t, "x"), "/Volumes/c/s/v/x.csv", map[string]any{"host": host, "token": "t"}, nil)
		if err == nil || !strings.Contains(err.Error(), "unencrypted") {
			t.Fatal(host, err)
		}
		if _, err := newDatabricks(map[string]any{"host": host, "http_path": "/sql/1.0/warehouses/a", "token": "t"}); err == nil ||
			!strings.Contains(err.Error(), "unencrypted") {
			t.Fatal(host, err)
		}
	}
}

// waitingDriver is a SQL connection whose every query runs until its context ends, announcing
// itself on queried.
type waitingDriver struct{ queried chan string }

func (d waitingDriver) Connect(context.Context) (driver.Conn, error) { return d, nil }
func (d waitingDriver) Driver() driver.Driver                        { return nil }
func (waitingDriver) Prepare(string) (driver.Stmt, error)            { return nil, errors.New("no") }
func (waitingDriver) Close() error                                   { return nil }
func (waitingDriver) Begin() (driver.Tx, error)                      { return nil, errors.New("no") }
func (d waitingDriver) QueryContext(ctx context.Context, q string, _ []driver.NamedValue) (driver.Rows, error) {
	d.queried <- q
	<-ctx.Done()
	return nil, ctx.Err()
}

func TestCancelStopsAStatementAndAnExplain(t *testing.T) {
	fake := waitingDriver{make(chan string, 1)}
	db := sql.OpenDB(fake)
	t.Cleanup(func() { db.Close() })
	role := sourceRole
	role.Open = func(map[string]any) (plugin.Session, error) {
		cn, err := db.Conn(context.Background())
		return &databricks{db: db, conn: cn}, err
	}
	c := plugintest.Start(t, pkg, role)
	c.Hello()
	c.Send(map[string]any{"type": "open", "id": 1, "connection": map[string]any{"host": "h", "http_path": "p"}, "read_only": false})
	if r := c.Reply(); r["type"] != "ok" {
		t.Fatalf("open: %v", r)
	}
	for id, req := range []map[string]any{{"type": "execute", "sql": "select 1"}, {"type": "check", "sql": "select 1"}} {
		req["id"] = id + 2
		c.Send(req)
		<-fake.queried
		c.Send(map[string]any{"type": "cancel", "id": id + 2})
		done := make(chan map[string]any)
		go func() { done <- c.Reply() }()
		select {
		case r := <-done:
			if r["type"] != "error" {
				t.Fatalf("%v: %v", req["type"], r)
			}
		case <-time.After(10 * time.Second):
			t.Fatalf("%v kept running after the cancel", req["type"])
		}
	}
}
