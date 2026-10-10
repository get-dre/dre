// Package plugin serves DRE's plugin protocol (docs/protocol.md) for the Go plugin packages:
// framing, the handshake, describe, validate, the request loop, and the error and exit rules.
//
// From protocol 1, every reply carries its request's id; stdin is read on a goroutine so a
// `cancel` reaches a running request (see Cancelled and Canceller); log with log/slog (Serve
// sends the default logger's records to core as `log` messages) and report Progress; return an
// *Error to give core a failure's kind and code.
//
// A package lists its roles. A source role opens a Session, which runs statements on one
// database session; a destination role delivers a file. Everything else (sign-in, the driver,
// the dialect) stays in the package. Results are normalised to DRE's type rule on the way out
// (see types.go), so every Go source sends the same Arrow types for the same kind of value.
//
// Each plugin pins its own Arrow version for its driver; this module uses arrow-go v18, and a
// plugin whose driver returns another Arrow version passes its batches through Arrow IPC.
package plugin

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"runtime/debug"
	"sort"
	"strings"
	"sync"
	"sync/atomic"

	"github.com/apache/arrow-go/v18/arrow"
	"github.com/apache/arrow-go/v18/arrow/array"
	"github.com/apache/arrow-go/v18/arrow/memory"
)

const (
	ProtocolMin = 0
	ProtocolMax = 1
)

// Error is a failure with its kind (one of the protocol's error kinds: config, plugin, refused,
// connection, auth, query, delivery, internal, cancelled, timed_out) and, optionally, a code.
// The code is namespaced by the plugin on the way out (`bad-token` is sent as
// `databricks/bad-token`).
type Error struct {
	Kind    string
	Code    string
	Message string
}

func (e *Error) Error() string { return e.Message }

// Canceller is implemented by a Session (or set as a Role's Cancel) that can stop a running
// request on the server, such as cancelling a statement. It's called on the stdin goroutine
// while the request runs.
type Canceller interface {
	Cancel()
}

// Field mirrors the protocol's `describe` entry for one profile field.
type Field struct {
	Name         string `json:"name"`
	Description  string `json:"description"`
	Required     bool   `json:"required"`
	Secret       bool   `json:"secret"`
	Default      any    `json:"default,omitempty"`
	SameAsSource string `json:"same_as_source,omitempty"`
	Manual       bool   `json:"manual,omitempty"`
}

// OptionField mirrors the protocol's `describe` entry for one option a report's config block
// takes (a destination entry's keys). Type is one of the protocol's option types; the Go SDK
// checks `string` (with Choices) and `boolean`, and accepts any value for the others.
type OptionField struct {
	Name        string   `json:"name"`
	Type        string   `json:"type"`
	Description string   `json:"description"`
	Required    bool     `json:"required,omitempty"`
	Default     any      `json:"default,omitempty"`
	Choices     []string `json:"choices,omitempty"`
}

// Role is one plugin a package serves.
type Role struct {
	Kind         string // "source" or "destination"
	Name         string
	Capabilities []string
	Fields       []Field
	// IdentifierQuote is the source's identifier quote character.
	IdentifierQuote string
	// Open starts a source session from the profile target's fields.
	Open func(conn map[string]any) (Session, error)
	// Options are what a report's config block for this role takes (checked before Deliver).
	Options []OptionField
	// Deliver sends one local file to remote, returning where it landed. opts is the
	// destination entry's options, already checked against Options.
	Deliver func(local, remote string, conn, opts map[string]any) (string, error)
	// Cancel, when set, stops a running delivery (see Canceller).
	Cancel func()
}

// ID is the role as the protocol writes a plugin: `<kind>/<name>`.
func (r Role) ID() string { return r.Kind + "/" + r.Name }

// Package is one plugin program: its version and every role it provides, in `provides` order.
type Package struct {
	Version string
	Roles   []Role
}

// RoleFor picks the role when core's hello doesn't name one, from the executable's name:
// dre-<kind>-* serves the first role of that kind, anything else the first role.
func (p Package) RoleFor(exe string) Role {
	base := strings.ToLower(filepath.Base(exe))
	for _, r := range p.Roles {
		if strings.HasPrefix(base, "dre-"+r.Kind+"-") {
			return r
		}
	}
	return p.Roles[0]
}

// Main serves the role the executable's name picks on stdin and stdout, and exits.
func Main(p Package) {
	os.Exit(Serve(os.Stdin, os.Stdout, p, p.RoleFor(os.Args[0])))
}

// Session is one database session: temp tables and settings made by one statement are there
// for the next. Tests use a fake.
type Session interface {
	// Run executes one statement and hands its result set to fn, or nil when the statement
	// returned none. The result is only valid inside fn.
	Run(sql string, fn func(Result) error) error
	// Check verifies a statement without running it. A non-empty note (e.g. the bytes a
	// statement would scan) is logged.
	Check(sql string) (note string, err error)
	// Load puts rows into a temporary table on the session. name is already checked to be a
	// plain identifier.
	Load(name string, schema *arrow.Schema, recs []arrow.Record) (Loaded, error)
	Close()
}

// Loaded is a Load's outcome; Warning is shown to the person when it isn't empty.
type Loaded struct {
	Relation string
	Rows     int64
	Warning  string
}

// Result is one statement's result set, batch by batch.
type Result interface {
	// Schema is the result's columns with the types the database declares for them; every
	// batch is converted to it (see Convert).
	Schema() (*arrow.Schema, error)
	HasNext() bool
	Next() (arrow.Record, error)
}

type server struct {
	pkg     Package
	role    Role
	frames  chan frameOrErr
	outMu   sync.Mutex
	out     *bufio.Writer
	db      Session
	greeted bool
	// version is the protocol version settled on (0 until hello).
	version atomic.Int32
	// current is the running request's id (0: none); cancelled the last one core cancelled.
	current   atomic.Uint64
	cancelled atomic.Uint64
	dbMu      sync.Mutex
}

type frameOrErr struct {
	f   Frame
	err error
}

// active is the server Serve is running, for Cancelled, Progress and the slog handler.
var active atomic.Pointer[server]

// Cancelled reports whether core has cancelled the request running now. Long loops check it
// between steps.
func Cancelled() bool {
	s := active.Load()
	if s == nil {
		return false
	}
	cur := s.current.Load()
	return cur != 0 && s.cancelled.Load() == cur
}

// Progress tells core how far a long request has got (protocol 1; logged at info before).
func Progress(message string, done, total int64) {
	s := active.Load()
	if s == nil {
		return
	}
	if s.version.Load() == 0 {
		fmt.Fprintf(os.Stderr, "info: %s (%d/%d)\n", message, done, total)
		return
	}
	m := map[string]any{"type": "progress", "done": done, "total": total}
	if message != "" {
		m["message"] = message
	}
	s.send(m)
}

// next is the next frame core sent, read by readInput.
func (s *server) next() (Frame, error) {
	fe, ok := <-s.frames
	if !ok {
		return Frame{}, ErrEOF
	}
	return fe.f, fe.err
}

// readInput reads stdin into s.frames, acting on `cancel` at once instead of queueing it behind
// the request it cancels. End of input cancels the running request too: core has gone.
func (s *server) readInput(in *bufio.Reader) {
	defer close(s.frames)
	for {
		f, err := ReadFrame(in)
		if err != nil {
			s.cancel(s.current.Load())
			s.frames <- frameOrErr{err: err}
			return
		}
		if f.Tag == TagJSON {
			var m struct {
				Type string `json:"type"`
				ID   uint64 `json:"id"`
			}
			if json.Unmarshal(f.Body, &m) == nil && m.Type == "cancel" {
				s.cancel(m.ID)
				continue
			}
		}
		s.frames <- frameOrErr{f: f}
	}
}

// cancel marks request id cancelled, and asks the session or role to stop it if it's running.
func (s *server) cancel(id uint64) {
	s.cancelled.Store(id)
	if id == 0 || s.current.Load() != id {
		return
	}
	s.dbMu.Lock()
	db := s.db
	s.dbMu.Unlock()
	if c, ok := db.(Canceller); ok {
		c.Cancel()
	}
	if s.role.Cancel != nil {
		s.role.Cancel()
	}
}

// slogHandler sends log records to core: `log` messages from protocol 1, stderr lines with the
// prefixes core shows before.
type slogHandler struct {
	attrs []slog.Attr
}

func (h slogHandler) Enabled(_ context.Context, l slog.Level) bool { return l >= slog.LevelDebug }

func (h slogHandler) Handle(_ context.Context, r slog.Record) error {
	s := active.Load()
	level := "debug"
	switch {
	case r.Level >= slog.LevelError:
		level = "error"
	case r.Level >= slog.LevelWarn:
		level = "warn"
	case r.Level >= slog.LevelInfo:
		level = "info"
	}
	fields := map[string]any{}
	add := func(a slog.Attr) bool {
		fields[a.Key] = a.Value.Resolve().Any()
		return true
	}
	for _, a := range h.attrs {
		add(a)
	}
	r.Attrs(add)
	if s == nil || s.version.Load() == 0 {
		prefix := map[string]string{"error": "warning: ", "warn": "warning: ", "info": "info: "}[level]
		fmt.Fprintln(os.Stderr, prefix+r.Message)
		return nil
	}
	m := map[string]any{"type": "log", "level": level, "message": r.Message}
	if len(fields) > 0 {
		m["fields"] = fields
	}
	s.send(m)
	return nil
}

func (h slogHandler) WithAttrs(as []slog.Attr) slog.Handler {
	return slogHandler{attrs: append(append([]slog.Attr{}, h.attrs...), as...)}
}

func (h slogHandler) WithGroup(string) slog.Handler { return h }

// exitCode ends Serve with a process exit code.
type exitCode int

// Serve speaks the protocol on stdin and stdout as role until core closes, and returns the
// process exit code.
func Serve(stdin io.Reader, stdout io.Writer, p Package, r Role) (code int) {
	s := &server{pkg: p, role: r, frames: make(chan frameOrErr, 16), out: bufio.NewWriter(stdout)}
	go s.readInput(bufio.NewReader(stdin))
	active.Store(s)
	prev := slog.Default()
	slog.SetDefault(slog.New(slogHandler{}))
	defer slog.SetDefault(prev)
	defer func() {
		if p := recover(); p != nil {
			if c, ok := p.(exitCode); ok {
				code = int(c)
				return
			}
			fmt.Fprintf(os.Stderr, "plugin panicked: %v\n%s", p, debug.Stack())
			s.send(map[string]any{"type": "error", "kind": "internal", "message": fmt.Sprintf("plugin panicked: %v", p)})
			code = 101
		}
	}()
	for {
		s.current.Store(0)
		f, err := s.next()
		if err == ErrEOF {
			s.closeDB()
			return 0
		}
		if err != nil {
			fmt.Fprintln(os.Stderr, err)
			s.send(errorReply(err))
			return 2
		}
		if f.Tag == TagArrow {
			s.send(errorMsg("unexpected Arrow frame"))
			continue
		}
		var req map[string]json.RawMessage
		if err := json.Unmarshal(f.Body, &req); err != nil {
			s.send(errorMsg("unsupported request `?`"))
			continue
		}
		var id uint64
		_ = json.Unmarshal(req["id"], &id)
		s.current.Store(id)
		t := str(req["type"])
		if t == "hello" {
			s.hello(req)
			continue
		}
		if !s.greeted {
			s.send(errorMsg("the first request must be `hello`"))
			continue
		}
		if t == "close" {
			s.closeDB()
			s.send(map[string]any{"type": "ok"})
			return 0
		}
		if Cancelled() {
			// Cancelled before it started: it never runs.
			s.send(s.errorReply(errors.New("cancelled before it started")))
			continue
		}
		if err := s.handle(t, req); err != nil {
			s.send(s.errorReply(err))
		}
	}
}

// send writes one message; from protocol 1 it carries the running request's id.
func (s *server) send(v map[string]any) {
	if id := s.current.Load(); id != 0 && s.version.Load() >= 1 {
		v["id"] = id
	}
	s.outMu.Lock()
	err := WriteJSON(s.out, v)
	s.outMu.Unlock()
	if err != nil {
		// Core has gone; nothing left to talk to.
		panic(exitCode(1))
	}
}

// errorReply is the `error` reply for a failed request: kind `cancelled` if core cancelled
// it, else an *Error's kind and code, else the message alone.
func (s *server) errorReply(err error) map[string]any {
	m := errorReply(err)
	var e *Error
	switch {
	case Cancelled():
		m["kind"] = "cancelled"
	case errors.As(err, &e):
		m["message"] = e.Message
		if e.Kind != "" {
			m["kind"] = e.Kind
		}
		if e.Code != "" {
			code := e.Code
			if !strings.Contains(code, "/") {
				code = s.role.Name + "/" + code
			}
			m["code"] = code
		}
	}
	return m
}

func (s *server) sendRecord(rec arrow.Record) error {
	b, err := EncodeRecord(rec)
	if err != nil {
		return err
	}
	s.outMu.Lock()
	err = WriteFrame(s.out, TagArrow, b)
	s.outMu.Unlock()
	if err != nil {
		panic(exitCode(1))
	}
	return nil
}

func (s *server) closeDB() {
	s.dbMu.Lock()
	defer s.dbMu.Unlock()
	if s.db != nil {
		s.db.Close()
		s.db = nil
	}
}

func (s *server) hello(req map[string]json.RawMessage) {
	var lo, hi int
	if json.Unmarshal(req["min_version"], &lo) != nil || json.Unmarshal(req["max_version"], &hi) != nil {
		s.send(errorMsg("unsupported request `hello`"))
		return
	}
	lo, hi = max(lo, ProtocolMin), min(hi, ProtocolMax)
	if lo > hi {
		s.send(map[string]any{"type": "version_mismatch", "min_version": ProtocolMin, "max_version": ProtocolMax})
		panic(exitCode(1))
	}
	provides := make([]string, len(s.pkg.Roles))
	for i, r := range s.pkg.Roles {
		provides[i] = r.ID()
	}
	if want := str(req["plugin"]); want != "" {
		found := false
		for _, r := range s.pkg.Roles {
			if r.ID() == want {
				s.role, found = r, true
			}
		}
		if !found {
			s.send(errorMsg(fmt.Sprintf("this executable provides %s, not %s", strings.Join(provides, ", "), want)))
			panic(exitCode(1))
		}
	}
	s.greeted = true
	s.version.Store(int32(hi))
	s.send(map[string]any{
		"type": "hello", "protocol_version": hi, "kind": s.role.Kind, "name": s.role.Name,
		"version": s.pkg.Version, "capabilities": s.role.Capabilities, "provides": provides,
	})
}

// optionErrors checks a config block of options against the role's Options: an unknown
// (misspelt) key is an error, not silently dropped. Values with Jinja (`{{`, `{%`) are rendered
// later, so they aren't checked.
func (s *server) optionErrors(opts map[string]any) []string {
	keys := make([]string, 0, len(opts))
	for k := range opts {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	known := make([]string, len(s.role.Options))
	for i, f := range s.role.Options {
		known[i] = f.Name
	}
	errs := []string{}
	for _, k := range keys {
		var f *OptionField
		for i := range s.role.Options {
			if s.role.Options[i].Name == k {
				f = &s.role.Options[i]
			}
		}
		switch {
		case f == nil && len(known) == 0:
			errs = append(errs, fmt.Sprintf("the `%s` %s takes no options, but got `%s`; check the key's spelling", s.role.Name, s.role.Kind, k))
		case f == nil:
			errs = append(errs, fmt.Sprintf("unknown option `%s` for %s `%s`; expected one of %s", k, s.role.Kind, s.role.Name, strings.Join(known, ", ")))
		default:
			if e := checkOption(*f, opts[k]); e != "" {
				errs = append(errs, fmt.Sprintf("`%s` %s", k, e))
			}
		}
	}
	for _, f := range s.role.Options {
		if v, ok := opts[f.Name]; f.Required && (!ok || v == nil) {
			errs = append(errs, fmt.Sprintf("`%s` is required", f.Name))
		}
	}
	return errs
}

func checkOption(f OptionField, v any) string {
	if v == nil {
		return ""
	}
	if str, ok := v.(string); ok && (strings.Contains(str, "{{") || strings.Contains(str, "{%")) {
		return ""
	}
	switch f.Type {
	case "string":
		str, ok := v.(string)
		if !ok {
			return "must be a string"
		}
		if len(f.Choices) > 0 {
			for _, c := range f.Choices {
				if c == str {
					return ""
				}
			}
			return "must be " + oneOf(f.Choices)
		}
	case "boolean":
		if _, ok := v.(bool); !ok {
			return "must be true or false"
		}
	}
	return ""
}

// oneOf reads as the Rust SDK's: `a` or `b`; one of `a`, `b`, `c`.
func oneOf(choices []string) string {
	q := make([]string, len(choices))
	for i, c := range choices {
		q[i] = "`" + c + "`"
	}
	if len(q) == 2 {
		return q[0] + " or " + q[1]
	}
	return "one of " + strings.Join(q, ", ")
}

func (s *server) handle(t string, req map[string]json.RawMessage) error {
	if t == "validate" {
		var r struct {
			Options map[string]any `json:"options"`
		}
		if json.Unmarshal(mustObject(req), &r) != nil {
			return fmt.Errorf("unsupported request `validate`")
		}
		s.send(map[string]any{"type": "validated", "errors": s.optionErrors(r.Options)})
		return nil
	}
	if s.role.Kind == "destination" {
		return s.handleDestination(t, req)
	}
	switch t {
	case "describe":
		s.send(map[string]any{"type": "describe", "connection_fields": fields(s.role), "identifier_quote": s.role.IdentifierQuote})
		return nil
	case "open":
		var conn map[string]any
		if err := json.Unmarshal(req["connection"], &conn); err != nil || conn == nil {
			return fmt.Errorf("unsupported request `open`")
		}
		s.closeDB()
		db, err := s.role.Open(conn)
		if err != nil {
			return err
		}
		s.dbMu.Lock()
		s.db = db
		s.dbMu.Unlock()
		s.send(map[string]any{"type": "ok"})
		return nil
	case "execute":
		var r struct {
			SQL      *string `json:"sql"`
			RowLimit *int64  `json:"row_limit"`
		}
		if json.Unmarshal(mustObject(req), &r) != nil || r.SQL == nil {
			return fmt.Errorf("unsupported request `execute`")
		}
		return s.execute(*r.SQL, r.RowLimit)
	case "check":
		var r struct {
			SQL *string `json:"sql"`
		}
		if json.Unmarshal(mustObject(req), &r) != nil || r.SQL == nil {
			return fmt.Errorf("unsupported request `check`")
		}
		if s.db == nil {
			return fmt.Errorf("no open session")
		}
		note, err := s.db.Check(*r.SQL)
		if err != nil {
			return err
		}
		if note != "" {
			slog.Debug(note)
		}
		s.send(map[string]any{"type": "ok"})
		return nil
	case "load":
		var name string
		if json.Unmarshal(req["name"], &name) != nil {
			return fmt.Errorf("unsupported request `load`")
		}
		return s.load(name)
	case "write", "deliver", "result_set_end", "finish":
		return fmt.Errorf("a source plugin doesn't handle %s requests", t)
	default:
		return fmt.Errorf("unsupported request `%s`", t)
	}
}

func fields(r Role) []Field {
	if r.Fields == nil {
		return []Field{}
	}
	return r.Fields
}

func (s *server) handleDestination(t string, req map[string]json.RawMessage) error {
	switch t {
	case "describe":
		d := map[string]any{"type": "describe", "connection_fields": fields(s.role)}
		if len(s.role.Options) > 0 {
			d["option_fields"] = s.role.Options
		}
		s.send(d)
		return nil
	case "deliver":
		var r struct {
			LocalPath  *string `json:"local_path"`
			RemotePath *string `json:"remote_path"`
			Files      []struct {
				LocalPath  string  `json:"local_path"`
				RemotePath *string `json:"remote_path"`
			} `json:"files"`
			Connection map[string]any `json:"connection"`
			Options    map[string]any `json:"options"`
		}
		if json.Unmarshal(mustObject(req), &r) != nil || r.Connection == nil {
			return fmt.Errorf("unsupported request `deliver`")
		}
		if errs := s.optionErrors(r.Options); len(errs) > 0 {
			return fmt.Errorf("%s", strings.Join(errs, "; "))
		}
		var local, remote string
		switch {
		case r.LocalPath != nil && len(r.Files) == 0:
			local = *r.LocalPath
			if r.RemotePath != nil {
				remote = *r.RemotePath
			}
		case r.LocalPath == nil && len(r.Files) == 1:
			local = r.Files[0].LocalPath
			if r.Files[0].RemotePath != nil {
				remote = *r.Files[0].RemotePath
			}
		case r.LocalPath == nil && len(r.Files) > 1:
			return fmt.Errorf("this destination takes one file per delivery")
		default:
			return fmt.Errorf("`deliver` needs exactly one of `local_path` or `files`")
		}
		if r.Options == nil {
			r.Options = map[string]any{}
		}
		loc, err := s.role.Deliver(local, remote, r.Connection, r.Options)
		if err != nil {
			return err
		}
		s.send(map[string]any{"type": "delivered", "location": loc})
		return nil
	case "open", "execute", "check", "load", "write", "result_set_end", "finish":
		return fmt.Errorf("a destination plugin doesn't handle %s requests", t)
	default:
		return fmt.Errorf("unsupported request `%s`", t)
	}
}

// execute runs one statement and streams its result, stopping at row_limit.
func (s *server) execute(sql string, rowLimit *int64) error {
	if s.db == nil {
		return fmt.Errorf("no open session")
	}
	return s.db.Run(sql, func(res Result) error {
		if res == nil {
			s.send(map[string]any{"type": "no_result"})
			return nil
		}
		return s.stream(res, rowLimit)
	})
}

// stream sends one result set: `result`, Arrow frames (at least one), `result_end`.
func (s *server) stream(res Result, rowLimit *int64) error {
	raw, err := res.Schema()
	if err != nil {
		return err
	}
	schema := OutputSchema(raw)
	cols := make([]string, len(schema.Fields()))
	for i, f := range schema.Fields() {
		cols[i] = f.Name
	}
	s.send(map[string]any{"type": "result", "columns": cols})
	var rows int64
	sent := false
	for res.HasNext() {
		if rowLimit != nil && rows >= *rowLimit {
			break
		}
		rec, err := res.Next()
		if err == io.EOF {
			break
		}
		if err != nil {
			// An error in place of result_end: the stream failed part-way.
			return err
		}
		out, err := Convert(rec, schema)
		rec.Release()
		if err != nil {
			return err
		}
		n := out.NumRows()
		if rowLimit != nil && rows+n > *rowLimit {
			sliced := out.NewSlice(0, *rowLimit-rows)
			out.Release()
			out, n = sliced, *rowLimit-rows
		}
		if n > 0 || !sent {
			if err := s.sendRecord(out); err != nil {
				out.Release()
				return err
			}
			rows += n
			sent = true
		}
		out.Release()
	}
	if !sent {
		b := array.NewRecordBuilder(memory.DefaultAllocator, schema)
		empty := b.NewRecord()
		b.Release()
		err := s.sendRecord(empty)
		empty.Release()
		if err != nil {
			return err
		}
	}
	s.send(map[string]any{"type": "result_end", "rows": rows})
	return nil
}

// load reads the rows core streams after `load` and hands them to the session.
func (s *server) load(name string) error {
	first, err := s.next()
	if err != nil {
		return err
	}
	if first.Tag != TagArrow {
		return fmt.Errorf("expected the rows to load, got %s", first.Body)
	}
	schema, recs, err := DecodeRecords(first.Body)
	if err != nil {
		return err
	}
	defer func() {
		for _, r := range recs {
			r.Release()
		}
	}()
	// Read to result_set_end before anything can fail, so the stream stays in step.
	for {
		f, err := s.next()
		if err != nil {
			return err
		}
		if f.Tag == TagArrow {
			_, more, err := DecodeRecords(f.Body)
			if err != nil {
				return err
			}
			recs = append(recs, more...)
			continue
		}
		var m map[string]json.RawMessage
		if json.Unmarshal(f.Body, &m) != nil || str(m["type"]) != "result_set_end" {
			return fmt.Errorf("expected result data, got %s", f.Body)
		}
		break
	}
	if !ValidName(name) {
		return fmt.Errorf("`%s` isn't a valid view name", name)
	}
	if s.db == nil {
		return fmt.Errorf("no open session")
	}
	l, err := s.db.Load(name, schema, recs)
	if err != nil {
		return err
	}
	reply := map[string]any{"type": "loaded", "relation": l.Relation, "rows": l.Rows}
	if l.Warning != "" {
		reply["warning"] = l.Warning
	}
	s.send(reply)
	return nil
}

// ValidName is true for a plain identifier: letters, digits and underscores.
func ValidName(name string) bool {
	if name == "" {
		return false
	}
	for _, r := range name {
		if !(r == '_' || r >= '0' && r <= '9' || r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z') {
			return false
		}
	}
	return true
}

func errorMsg(m string) map[string]any { return map[string]any{"type": "error", "message": m} }

func errorReply(err error) map[string]any { return errorMsg(err.Error()) }

func str(raw json.RawMessage) string {
	var s string
	_ = json.Unmarshal(raw, &s)
	return s
}

func mustObject(m map[string]json.RawMessage) []byte {
	b, _ := json.Marshal(m)
	return b
}
