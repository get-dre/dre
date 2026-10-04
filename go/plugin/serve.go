// Package plugin serves DRE's plugin protocol (docs/protocol.md) for the Go plugin packages:
// framing, the handshake, describe, validate, the request loop, and the error and exit rules.
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
	"encoding/json"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"runtime/debug"
	"sort"
	"strings"

	"github.com/apache/arrow-go/v18/arrow"
	"github.com/apache/arrow-go/v18/arrow/array"
	"github.com/apache/arrow-go/v18/arrow/memory"
)

const (
	ProtocolMin = 0
	ProtocolMax = 0
)

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
	// Deliver sends one local file to remote, returning where it landed.
	Deliver func(local, remote string, conn map[string]any) (string, error)
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
	in      *bufio.Reader
	out     *bufio.Writer
	db      Session
	greeted bool
}

// exitCode ends Serve with a process exit code.
type exitCode int

// Serve speaks the protocol on stdin and stdout as role until core closes, and returns the
// process exit code.
func Serve(stdin io.Reader, stdout io.Writer, p Package, r Role) (code int) {
	s := &server{pkg: p, role: r, in: bufio.NewReader(stdin), out: bufio.NewWriter(stdout)}
	defer func() {
		if p := recover(); p != nil {
			if c, ok := p.(exitCode); ok {
				code = int(c)
				return
			}
			fmt.Fprintf(os.Stderr, "plugin panicked: %v\n%s", p, debug.Stack())
			s.send(map[string]any{"type": "error", "message": fmt.Sprintf("plugin panicked: %v", p)})
			code = 101
		}
	}()
	for {
		f, err := ReadFrame(s.in)
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
		if err := s.handle(t, req); err != nil {
			s.send(errorReply(err))
		}
	}
}

func (s *server) send(v any) {
	if err := WriteJSON(s.out, v); err != nil {
		// Core has gone; nothing left to talk to.
		panic(exitCode(1))
	}
}

func (s *server) sendRecord(rec arrow.Record) error {
	b, err := EncodeRecord(rec)
	if err != nil {
		return err
	}
	if err := WriteFrame(s.out, TagArrow, b); err != nil {
		panic(exitCode(1))
	}
	return nil
}

func (s *server) closeDB() {
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
	s.send(map[string]any{
		"type": "hello", "protocol_version": hi, "kind": s.role.Kind, "name": s.role.Name,
		"version": s.pkg.Version, "capabilities": s.role.Capabilities, "provides": provides,
	})
}

// optionErrors checks a config block of options. No Go role takes options, so every key is
// refused and a misspelt one is an error, not silently dropped.
func (s *server) optionErrors(opts map[string]any) []string {
	keys := make([]string, 0, len(opts))
	for k := range opts {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	errs := []string{}
	for _, k := range keys {
		errs = append(errs, fmt.Sprintf("the `%s` %s takes no options, but got `%s`; check the key's spelling", s.role.Name, s.role.Kind, k))
	}
	return errs
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
		s.db = db
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
			fmt.Fprintln(os.Stderr, note)
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
		s.send(map[string]any{"type": "describe", "connection_fields": fields(s.role)})
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
		loc, err := s.role.Deliver(local, remote, r.Connection)
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
	first, err := ReadFrame(s.in)
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
		f, err := ReadFrame(s.in)
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
