package plugin_test

// The protocol loop against a fake session: framing, the handshake, every source and
// destination request, and the error and exit rules (docs/protocol.md). Each plugin's own
// driver is covered by its package's tests and by crates/dre-protocol/tests/go_plugins.rs.

import (
	"bufio"
	"bytes"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"strings"
	"testing"
	"time"

	"github.com/apache/arrow-go/v18/arrow"
	"github.com/apache/arrow-go/v18/arrow/array"
	"github.com/apache/arrow-go/v18/arrow/memory"

	"github.com/get-dre/dre/go/plugin"
	"github.com/get-dre/dre/go/plugin/plugintest"
)

type fakeResult struct {
	schema *arrow.Schema
	recs   []arrow.Record
	i      int
}

func (r *fakeResult) Schema() (*arrow.Schema, error) { return r.schema, nil }
func (r *fakeResult) HasNext() bool                  { return r.i < len(r.recs) }
func (r *fakeResult) Next() (arrow.Record, error) {
	if r.i >= len(r.recs) {
		return nil, io.EOF
	}
	rec := r.recs[r.i]
	r.i++
	rec.Retain()
	return rec, nil
}

type fakeDB struct {
	ran    []string
	loaded []string
	closed bool
	stop   chan struct{}
}

func (f *fakeDB) Cancel() { close(f.stop) }

var idSchema = arrow.NewSchema([]arrow.Field{{Name: "id", Type: arrow.PrimitiveTypes.Int64, Nullable: true}}, nil)

func ids(from, n int64) arrow.Record {
	b := array.NewInt64Builder(memory.DefaultAllocator)
	defer b.Release()
	for i := from; i < from+n; i++ {
		b.Append(i)
	}
	col := b.NewArray()
	defer col.Release()
	return array.NewRecord(idSchema, []arrow.Array{col}, n)
}

func (f *fakeDB) Run(sql string, fn func(plugin.Result) error) error {
	f.ran = append(f.ran, sql)
	switch {
	case strings.HasPrefix(strings.ToLower(sql), "create"):
		return fn(nil)
	case sql == "select three batches":
		return fn(&fakeResult{schema: idSchema, recs: []arrow.Record{ids(0, 2), ids(2, 2), ids(4, 2)}})
	case sql == "select nothing":
		return fn(&fakeResult{schema: idSchema})
	case sql == "select null":
		s := arrow.NewSchema([]arrow.Field{{Name: "n", Type: arrow.Null, Nullable: true}}, nil)
		col := array.NewNull(2)
		defer col.Release()
		return fn(&fakeResult{schema: s, recs: []arrow.Record{array.NewRecord(s, []arrow.Array{col}, 2)}})
	}
	switch sql {
	case "wait":
		// Runs until cancelled.
		select {
		case <-f.stop:
			return errors.New("statement cancelled")
		case <-time.After(20 * time.Second):
			return fn(nil)
		}
	case "log":
		slog.Info("hello", "attempt", 2)
		plugin.Progress("reading", 1, 2)
		return fn(nil)
	case "coded":
		return &plugin.Error{Kind: "auth", Code: "bad-token", Message: "the token was refused"}
	}
	return errors.New("syntax error")
}

func (f *fakeDB) Check(sql string) (string, error) {
	if strings.Contains(sql, "nope") {
		return "", errors.New("column `nope` cannot be resolved")
	}
	return "would scan 10 bytes", nil
}

func (f *fakeDB) Load(name string, schema *arrow.Schema, recs []arrow.Record) (plugin.Loaded, error) {
	var n int64
	for _, r := range recs {
		n += r.NumRows()
	}
	f.loaded = append(f.loaded, fmt.Sprintf("%s %s %d", name, schema.Field(0).Name, n))
	return plugin.Loaded{Relation: "tmp_" + name, Rows: n}, nil
}

func (f *fakeDB) Close() { f.closed = true }

func pkg(db *fakeDB, delivered *[]string) plugin.Package {
	src := plugin.Role{
		Kind: "source", Name: "fake", Capabilities: []string{"sessions", "check", "load", "validate"},
		Fields:          []plugin.Field{{Name: "host", Description: "h", Required: true}, {Name: "token", Description: "t", Secret: true}},
		IdentifierQuote: "`",
		Open:            func(map[string]any) (plugin.Session, error) { return db, nil },
	}
	dst := plugin.Role{
		Kind: "destination", Name: "fake", Capabilities: []string{"validate"},
		Fields: []plugin.Field{{Name: "host", Description: "h", Required: true}},
		Deliver: func(local, remote string, _, _ map[string]any) (string, error) {
			if remote == "" {
				return "", errors.New("needs a path")
			}
			*delivered = append(*delivered, local+" -> "+remote)
			return "fake:" + remote, nil
		},
	}
	return plugin.Package{Version: "1.2.3", Roles: []plugin.Role{src, dst}}
}

func start(t *testing.T) (*plugintest.Conversation, *fakeDB) {
	db := &fakeDB{stop: make(chan struct{})}
	p := pkg(db, &[]string{})
	return plugintest.Start(t, p, p.Roles[0]), db
}

func TestHandshakeDescribeAndRequestErrors(t *testing.T) {
	c, _ := start(t)
	c.Send(map[string]any{"type": "describe"})
	plugintest.ExpectError(t, c.Reply(), "the first request must be `hello`")
	h := c.Hello()
	if h["name"] != "fake" || h["kind"] != "source" || h["version"] != "1.2.3" || fmt.Sprint(h["provides"]) != "[source/fake destination/fake]" {
		t.Fatalf("hello: %v", h)
	}
	c.Send(map[string]any{"type": "describe"})
	d := c.Reply()
	if plugintest.FieldNames(d) != "host,token" || d["identifier_quote"] != "`" {
		t.Fatalf("describe: %v", d)
	}
	c.Send(map[string]any{"type": "nonsense"})
	plugintest.ExpectError(t, c.Reply(), "unsupported request `nonsense`")
	c.Send(map[string]any{"type": "write", "path": "x", "format": "csv", "options": map[string]any{}, "result_sets": []any{}})
	plugintest.ExpectError(t, c.Reply(), "a source plugin doesn't handle write requests")
	c.SendRecord(ids(0, 1))
	plugintest.ExpectError(t, c.Reply(), "unexpected Arrow frame")
	c.Send(map[string]any{"type": "execute", "sql": "select 1"})
	plugintest.ExpectError(t, c.Reply(), "no open session")
	c.Send(map[string]any{"type": "validate", "options": map[string]any{"x": 1}})
	if r := c.Reply(); r["type"] != "validated" || len(r["errors"].([]any)) != 1 {
		t.Fatalf("%v", r)
	}
	c.Send(map[string]any{"type": "close"})
	if r := c.Reply(); r["type"] != "ok" {
		t.Fatalf("close: %v", r)
	}
	if code := <-c.Code; code != 0 {
		t.Fatalf("exit %d", code)
	}
}

func TestExecuteStreamsResultsWithinTheRowLimit(t *testing.T) {
	c, db := start(t)
	c.Hello()
	c.Open(map[string]any{})
	// No result set.
	c.Send(map[string]any{"type": "execute", "sql": "create temp view v"})
	if r := c.Reply(); r["type"] != "no_result" {
		t.Fatalf("%v", r)
	}
	// Three batches, all sent.
	c.Send(map[string]any{"type": "execute", "sql": "select three batches"})
	if r := c.Reply(); r["type"] != "result" || r["columns"].([]any)[0] != "id" {
		t.Fatalf("%v", r)
	}
	for range 3 {
		c.Records()
	}
	if r := c.Reply(); r["type"] != "result_end" || r["rows"] != 6.0 {
		t.Fatalf("%v", r)
	}
	// row_limit 3: a batch and a slice, then stop.
	c.Send(map[string]any{"type": "execute", "sql": "select three batches", "row_limit": 3})
	c.Reply()
	a, b := c.Records(), c.Records()
	if a[0].NumRows() != 2 || b[0].NumRows() != 1 || b[0].Column(0).(*array.Int64).Value(0) != 2 {
		t.Fatalf("limited batches: %d, %d", a[0].NumRows(), b[0].NumRows())
	}
	if r := c.Reply(); r["rows"] != 3.0 {
		t.Fatalf("%v", r)
	}
	// Zero rows: still one frame carrying the columns.
	c.Send(map[string]any{"type": "execute", "sql": "select nothing"})
	c.Reply()
	if recs := c.Records(); len(recs) != 1 || recs[0].NumRows() != 0 || recs[0].Schema().Field(0).Name != "id" {
		t.Fatalf("empty result: %v", recs)
	}
	if r := c.Reply(); r["rows"] != 0.0 {
		t.Fatalf("%v", r)
	}
	// A NULL-typed column arrives as text.
	c.Send(map[string]any{"type": "execute", "sql": "select null"})
	c.Reply()
	if recs := c.Records(); recs[0].Schema().Field(0).Type.ID() != arrow.STRING || recs[0].Column(0).NullN() != 2 {
		t.Fatalf("null column: %v", recs[0].Schema())
	}
	c.Reply()
	// An error, and the plugin keeps serving.
	c.Send(map[string]any{"type": "execute", "sql": "bad sql"})
	plugintest.ExpectError(t, c.Reply(), "syntax error")
	c.Send(map[string]any{"type": "check", "sql": "select 1"})
	if r := c.Reply(); r["type"] != "ok" {
		t.Fatalf("%v", r)
	}
	// The check's note is logged (at debug: shown with -v).
	if l := c.Logs[len(c.Logs)-1]; l["level"] != "debug" || l["message"] != "would scan 10 bytes" {
		t.Fatalf("%v", l)
	}
	c.Send(map[string]any{"type": "check", "sql": "select nope"})
	plugintest.ExpectError(t, c.Reply(), "`nope` cannot be resolved")
	// End of input closes the session and exits 0.
	c.CloseInput()
	if code := <-c.Code; code != 0 || !db.closed {
		t.Fatalf("exit %d, closed %v", code, db.closed)
	}
}

func TestLoadHandsEveryBatchToTheSession(t *testing.T) {
	c, db := start(t)
	c.Hello()
	c.Open(map[string]any{})
	c.Send(map[string]any{"type": "load", "name": "countries"})
	c.SendRecord(ids(0, 2))
	c.SendRecord(ids(2, 1))
	c.Send(map[string]any{"type": "result_set_end"})
	r := c.Reply()
	if r["type"] != "loaded" || r["relation"] != "tmp_countries" || r["rows"] != 3.0 || r["warning"] != nil {
		t.Fatalf("%v", r)
	}
	if fmt.Sprint(db.loaded) != "[countries id 3]" {
		t.Fatalf("%v", db.loaded)
	}
	// A bad name is refused after the rows are read, so the stream stays in step.
	c.Send(map[string]any{"type": "load", "name": "no-dashes"})
	c.SendRecord(ids(0, 1))
	c.Send(map[string]any{"type": "result_set_end"})
	plugintest.ExpectError(t, c.Reply(), "isn't a valid view name")
	c.Send(map[string]any{"type": "describe"})
	if r := c.Reply(); r["type"] != "describe" {
		t.Fatalf("out of step: %v", r)
	}
}

func TestMalformedFramesAndVersionMismatchExit(t *testing.T) {
	c, _ := start(t)
	c.SendRaw([]byte{0, 0, 0, 2, 'X', 0})
	plugintest.ExpectError(t, c.Reply(), "unknown frame type byte 0x58")
	if code := <-c.Code; code != 2 {
		t.Fatalf("exit %d", code)
	}
	c, _ = start(t)
	c.Send(map[string]any{"type": "hello", "min_version": 5, "max_version": 6, "core_version": "x"})
	if r := c.Reply(); r["type"] != "version_mismatch" || r["max_version"] != 1.0 {
		t.Fatalf("%v", r)
	}
	if code := <-c.Code; code != 1 {
		t.Fatalf("exit %d", code)
	}
}

func TestFramesRoundTripAndRejectBadLengths(t *testing.T) {
	var buf bytes.Buffer
	w := bufio.NewWriter(&buf)
	plugin.WriteJSON(w, map[string]any{"type": "ok"})
	b, _ := plugin.EncodeRecord(ids(0, 3))
	plugin.WriteFrame(w, plugin.TagArrow, b)
	f, err := plugin.ReadFrame(&buf)
	if err != nil || f.Tag != plugin.TagJSON || string(f.Body) != `{"type":"ok"}` {
		t.Fatalf("%v %s", err, f.Body)
	}
	f, _ = plugin.ReadFrame(&buf)
	if _, recs, err := plugin.DecodeRecords(f.Body); err != nil || recs[0].NumRows() != 3 {
		t.Fatalf("%v", err)
	}
	if _, err := plugin.ReadFrame(&buf); err != plugin.ErrEOF {
		t.Fatalf("clean end: %v", err)
	}
	if _, err := plugin.ReadFrame(bytes.NewReader([]byte{0, 0, 0, 0})); err == nil {
		t.Fatal("zero length accepted")
	}
	if _, err := plugin.ReadFrame(bytes.NewReader([]byte{0, 0, 0, 9, 'J', '{'})); err == nil || err == plugin.ErrEOF {
		t.Fatalf("truncated body: %v", err)
	}
}

func TestHelloServesThePluginCoreAsksFor(t *testing.T) {
	db := &fakeDB{stop: make(chan struct{})}
	p := pkg(db, &[]string{})
	if p.RoleFor("/x/dre-destination-fake.exe").ID() != "destination/fake" || p.RoleFor("dre-plugin-fake").ID() != "source/fake" {
		t.Fatal("RoleFor")
	}
	c := plugintest.Start(t, p, p.Roles[0])
	c.Send(map[string]any{"type": "hello", "min_version": 0, "max_version": 0, "core_version": "t", "plugin": "destination/fake"})
	if r := c.Reply(); r["kind"] != "destination" {
		t.Fatalf("%v", r)
	}
	c = plugintest.Start(t, p, p.Roles[0])
	c.Send(map[string]any{"type": "hello", "min_version": 0, "max_version": 0, "core_version": "t", "plugin": "destination/other"})
	plugintest.ExpectError(t, c.Reply(), "this executable provides source/fake, destination/fake, not destination/other")
	if code := <-c.Code; code != 1 {
		t.Fatalf("exit code %d", code)
	}
}

func TestTheDestinationDeliversOneFile(t *testing.T) {
	var delivered []string
	p := pkg(&fakeDB{}, &delivered)
	c := plugintest.Start(t, p, p.Roles[1])
	c.Hello()
	c.Send(map[string]any{"type": "describe"})
	if d := c.Reply(); plugintest.FieldNames(d) != "host" || d["identifier_quote"] != nil {
		t.Fatalf("%v", d)
	}
	c.Send(map[string]any{"type": "deliver", "local_path": "/x", "remote_path": "r", "connection": map[string]any{}, "options": map[string]any{"to": "x"}})
	plugintest.ExpectError(t, c.Reply(), "the `fake` destination takes no options, but got `to`")
	c.Send(map[string]any{"type": "execute", "sql": "select 1"})
	plugintest.ExpectError(t, c.Reply(), "a destination plugin doesn't handle execute requests")
	c.Send(map[string]any{"type": "deliver", "files": []any{map[string]any{"local_path": "/a", "remote_path": "b"}}, "connection": map[string]any{}, "options": map[string]any{}})
	if r := c.Reply(); r["type"] != "delivered" || r["location"] != "fake:b" {
		t.Fatalf("%v", r)
	}
	c.Send(map[string]any{"type": "deliver", "local_path": "/a", "connection": map[string]any{}, "options": map[string]any{}})
	plugintest.ExpectError(t, c.Reply(), "needs a path")
	if fmt.Sprint(delivered) != "[/a -> b]" {
		t.Fatalf("%v", delivered)
	}
}

func TestProtocolOneIdsLogsErrorsAndCancel(t *testing.T) {
	c, _ := start(t)
	c.Hello()
	c.Send(map[string]any{"type": "open", "id": 1, "connection": map[string]any{}, "read_only": false})
	if r := c.Reply(); r["type"] != "ok" || r["id"] != 1.0 {
		t.Fatalf("open: %v", r)
	}
	// A cancel for a request that isn't running is ignored.
	c.Send(map[string]any{"type": "cancel", "id": 99})
	c.Send(map[string]any{"type": "execute", "id": 2, "sql": "log"})
	if r := c.Message(); r["type"] != "log" || r["id"] != 2.0 || r["level"] != "info" || r["message"] != "hello" ||
		fmt.Sprint(r["fields"]) != "map[attempt:2]" {
		t.Fatalf("log: %v", r)
	}
	if r := c.Message(); r["type"] != "progress" || r["done"] != 1.0 || r["total"] != 2.0 || r["message"] != "reading" {
		t.Fatalf("progress: %v", r)
	}
	if r := c.Reply(); r["type"] != "no_result" || r["id"] != 2.0 {
		t.Fatalf("execute: %v", r)
	}
	c.Send(map[string]any{"type": "execute", "id": 3, "sql": "coded"})
	if r := c.Reply(); r["type"] != "error" || r["kind"] != "auth" || r["code"] != "fake/bad-token" || r["id"] != 3.0 {
		t.Fatalf("coded: %v", r)
	}
	c.Send(map[string]any{"type": "execute", "id": 4, "sql": "wait"})
	time.Sleep(100 * time.Millisecond)
	start := time.Now()
	c.Send(map[string]any{"type": "cancel", "id": 4})
	if r := c.Reply(); r["type"] != "error" || r["kind"] != "cancelled" || r["id"] != 4.0 {
		t.Fatalf("cancel: %v", r)
	}
	if time.Since(start) > 5*time.Second {
		t.Fatal("the cancel took too long")
	}
	c.Send(map[string]any{"type": "describe", "id": 5})
	if r := c.Reply(); r["type"] != "describe" || r["id"] != 5.0 {
		t.Fatalf("after cancel: %v", r)
	}
}

func TestDeclaredOptionsAreDescribedCheckedAndPassedOn(t *testing.T) {
	var got map[string]any
	p := plugin.Package{Version: "1", Roles: []plugin.Role{{
		Kind: "destination", Name: "fake", Options: plugin.DeliveryOptions()[:2],
		Deliver: func(_, remote string, _, opts map[string]any) (string, error) {
			got = opts
			return remote, nil
		},
	}}}
	c := plugintest.Start(t, p, p.Roles[0])
	c.Hello()
	c.Send(map[string]any{"type": "describe"})
	if d := c.Reply(); len(d["option_fields"].([]any)) != 2 {
		t.Fatalf("%v", d)
	}
	c.Send(map[string]any{"type": "validate", "options": map[string]any{"if_exists": "keep", "atomic": "yes", "tmp": 1}})
	r := c.Reply()
	errs := fmt.Sprint(r["errors"])
	for _, want := range []string{"`atomic` must be true or false", "`if_exists` must be one of `overwrite`, `error`, `number`", "unknown option `tmp` for destination `fake`; expected one of if_exists, atomic"} {
		if !strings.Contains(errs, want) {
			t.Fatalf("%q lacks %q", errs, want)
		}
	}
	// Jinja is rendered later, so it isn't checked.
	c.Send(map[string]any{"type": "validate", "options": map[string]any{"if_exists": "{{ var('mode') }}"}})
	if r := c.Reply(); len(r["errors"].([]any)) != 0 {
		t.Fatalf("%v", r)
	}
	c.Send(map[string]any{"type": "deliver", "local_path": "/a", "remote_path": "b", "connection": map[string]any{}, "options": map[string]any{"if_exists": "number"}})
	if r := c.Reply(); r["type"] != "delivered" || got["if_exists"] != "number" {
		t.Fatalf("%v %v", r, got)
	}
}
