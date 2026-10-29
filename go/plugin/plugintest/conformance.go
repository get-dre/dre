package plugintest

// The Go runner of the language-neutral conformance cases (crates/dre-protocol/conformance/
// protocol.json): each case is a request sequence and the properties its replies must have. It
// serves the package in-process, a fresh Serve per case, and checks every message against the
// published schema. The Rust runner (dre_protocol::conformance) runs the same cases against a
// plugin binary. The case format is described in docs/protocol.md.

import (
	"bufio"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math"
	"os"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/apache/arrow-go/v18/arrow"
	"github.com/apache/arrow-go/v18/arrow/array"
	"github.com/apache/arrow-go/v18/arrow/memory"

	"github.com/get-dre/dre/go/plugin"
)

// replyTimeout is how long a plugin may take over one reply.
const replyTimeout = 20 * time.Second

// Session is what the session cases run: a connection that opens, and statements by role
// (`rows`, `empty`, `no_result`, `error`, `slow`; see docs/protocol.md).
type Session struct {
	Connection map[string]any
	Statements map[string]string
}

// Options say how to run the suite.
type Options struct {
	// Session, when set, runs the session cases (sources).
	Session *Session
}

// Check is one case's outcome for one plugin.
type Check struct {
	Plugin string
	Name   string
	// Passed is true for a case that passed or didn't apply.
	Passed bool
	Detail string
	// Skipped says why the case didn't apply; empty when it ran.
	Skipped string
}

type conformanceCase struct {
	Name     string           `json:"name"`
	Scope    string           `json:"scope"`
	Kinds    []string         `json:"kinds"`
	Requires []string         `json:"requires"`
	Session  []string         `json:"session"`
	Steps    []map[string]any `json:"steps"`
}

// Conformance fails t unless every case passes for every role of p.
func Conformance(t *testing.T, p plugin.Package, o Options) {
	t.Helper()
	for _, c := range RunConformance(t, p, o) {
		if !c.Passed {
			t.Errorf("%s: %s: %s", c.Plugin, c.Name, c.Detail)
		}
	}
}

// RunConformance runs every case for every role of p.
func RunConformance(t testing.TB, p plugin.Package, o Options) []Check {
	t.Helper()
	var file struct {
		Cases []conformanceCase `json:"cases"`
	}
	if err := json.Unmarshal(CaseFile(t, "protocol.json"), &file); err != nil {
		t.Fatalf("protocol.json: %v", err)
	}
	r := &runner{schema: Schema(t), pkg: p, opts: o}
	var out []Check
	for _, c := range file.Cases {
		if c.Scope == "package" {
			out = append(out, r.finish("package", c, r.runCase(nil, c)))
		}
	}
	for i := range p.Roles {
		role := p.Roles[i]
		for _, c := range file.Cases {
			if c.Scope != "" {
				continue
			}
			if why := r.notApplicable(c, role); why != "" {
				out = append(out, Check{Plugin: role.ID(), Name: c.Name, Passed: true, Skipped: why})
				continue
			}
			out = append(out, r.finish(role.ID(), c, r.runCase(&role, c)))
		}
	}
	return out
}

type runner struct {
	schema *SchemaDoc
	pkg    plugin.Package
	opts   Options
}

func (r *runner) finish(id string, c conformanceCase, err error) Check {
	ch := Check{Plugin: id, Name: c.Name, Passed: err == nil}
	if err != nil {
		ch.Detail = err.Error()
	}
	return ch
}

func (r *runner) notApplicable(c conformanceCase, role plugin.Role) string {
	if len(c.Kinds) > 0 && !slices.Contains(c.Kinds, role.Kind) {
		return fmt.Sprintf("for %v plugins", c.Kinds)
	}
	for _, cap := range c.Requires {
		if !slices.Contains(role.Capabilities, cap) {
			return fmt.Sprintf("the plugin doesn't advertise `%s`", cap)
		}
	}
	if len(c.Session) == 0 {
		return ""
	}
	if r.opts.Session == nil {
		return "needs a session (Options.Session)"
	}
	for _, s := range c.Session {
		if _, ok := r.opts.Session.Statements[s]; !ok {
			return fmt.Sprintf("the session has no `%s` statement", s)
		}
	}
	return ""
}

func (r *runner) vars(role *plugin.Role, tmp string) map[string]any {
	x := r.schema.Protocol()
	v := map[string]any{
		"tmp":          tmp,
		"min_version":  float64(plugin.ProtocolMin),
		"max_version":  float64(plugin.ProtocolMax),
		"far_version":  float64(plugin.ProtocolMax + 1000),
		"capabilities": toAny(x.Capabilities),
		"error_kinds":  toAny(x.ErrorKinds),
		"connection":   map[string]any{},
	}
	if role != nil {
		v["kind"], v["name"], v["plugin"] = role.Kind, role.Name, role.ID()
	}
	if s := r.opts.Session; s != nil {
		if s.Connection != nil {
			v["connection"] = s.Connection
		}
		for k, sql := range s.Statements {
			v["sql."+k] = sql
		}
	}
	return v
}

func toAny(s []string) []any {
	out := make([]any, len(s))
	for i, x := range s {
		out[i] = x
	}
	return out
}

func (r *runner) runCase(role *plugin.Role, c conformanceCase) (err error) {
	tmp, err := os.MkdirTemp("", "dre-conformance-")
	if err != nil {
		return err
	}
	defer os.RemoveAll(tmp)
	vars := r.vars(role, tmp)
	d := r.spawn(role)
	defer func() {
		if stopErr := d.stop(); err == nil {
			err = stopErr
		}
	}()
	if len(c.Steps) == 0 || c.Steps[0]["hello"] == nil {
		if err := d.writeJSON(hello(role, nil)); err != nil {
			return err
		}
		h, err := d.reply()
		if err != nil {
			return fmt.Errorf("handshake: %w", err)
		}
		if h["type"] != "hello" {
			return fmt.Errorf("handshake: expected a hello reply, got %s", show(h))
		}
	}
	for i, step := range c.Steps {
		s, err := substitute(step, vars)
		if err != nil {
			return err
		}
		if err := d.step(role, s.(map[string]any)); err != nil {
			return fmt.Errorf("step %d: %w", i+1, err)
		}
	}
	return nil
}

func hello(role *plugin.Role, with map[string]any) map[string]any {
	h := map[string]any{"type": "hello", "min_version": plugin.ProtocolMin, "max_version": plugin.ProtocolMax, "core_version": "conformance"}
	if role != nil {
		h["plugin"] = role.ID()
	}
	for k, v := range with {
		h[k] = v
	}
	return h
}

// driver is one in-process plugin: Serve on pipes, read frame by frame.
type driver struct {
	schema *SchemaDoc
	in     *io.PipeWriter
	frames chan frameOrErr
	code   chan int
	exited *int
	last   map[string]any
}

type frameOrErr struct {
	f   plugin.Frame
	err error
}

func (r *runner) spawn(role *plugin.Role) *driver {
	inR, inW := io.Pipe()
	outR, outW := io.Pipe()
	d := &driver{schema: r.schema, in: inW, frames: make(chan frameOrErr, 256), code: make(chan int, 1)}
	serve := r.pkg.Roles[0]
	if role != nil {
		serve = *role
	}
	go func() {
		code := plugin.Serve(inR, outW, r.pkg, serve)
		outW.Close()
		inR.CloseWithError(errors.New("the plugin has exited"))
		d.code <- code
	}()
	go func() {
		br := bufio.NewReader(outR)
		for {
			f, err := plugin.ReadFrame(br)
			d.frames <- frameOrErr{f, err}
			if err != nil {
				return
			}
		}
	}()
	return d
}

// stop ends the plugin and waits for Serve to return, so the next case's Serve is the only one
// (Serve keeps package-wide state); a plugin still running after end of input fails the case.
func (d *driver) stop() error {
	d.in.Close()
	if _, ok := d.wait(replyTimeout); !ok {
		return errors.New("still running after end of input")
	}
	return nil
}

func (d *driver) wait(timeout time.Duration) (int, bool) {
	if d.exited != nil {
		return *d.exited, true
	}
	select {
	case c := <-d.code:
		d.exited = &c
		return c, true
	case <-time.After(timeout):
		return 0, false
	}
}

func (d *driver) writeJSON(v any) error {
	b, err := json.Marshal(v)
	if err != nil {
		return err
	}
	return d.writeRaw(FrameBytes(plugin.TagJSON, b))
}

// writeRaw writes without blocking the case on a plugin that stopped reading.
func (d *driver) writeRaw(b []byte) error {
	done := make(chan error, 1)
	go func() { _, err := d.in.Write(b); done <- err }()
	select {
	case err := <-done:
		return err
	case <-time.After(replyTimeout):
		return errors.New("the plugin stopped reading its input")
	}
}

func (d *driver) frame() (plugin.Frame, error) {
	select {
	case fe := <-d.frames:
		if fe.err != nil {
			// Keep the error for later reads.
			d.frames <- fe
		}
		return fe.f, fe.err
	case <-time.After(replyTimeout):
		return plugin.Frame{}, fmt.Errorf("no reply within %s", replyTimeout)
	}
}

// message decodes a JSON frame and checks it against the schema.
func (d *driver) message(f plugin.Frame) (map[string]any, error) {
	var m map[string]any
	if err := json.Unmarshal(f.Body, &m); err != nil {
		return nil, fmt.Errorf("a reply that isn't a JSON object: %v", err)
	}
	if err := d.schema.CheckResponse(m); err != nil {
		return nil, fmt.Errorf("%s doesn't match the message schema: %v", show(m), err)
	}
	return m, nil
}

// reply is the next message that isn't a log or progress.
func (d *driver) reply() (map[string]any, error) {
	for {
		f, err := d.frame()
		if err != nil {
			return nil, err
		}
		if f.Tag != plugin.TagJSON {
			return nil, errors.New("Arrow data where a reply was expected")
		}
		m, err := d.message(f)
		if err != nil {
			return nil, err
		}
		if m["type"] != "log" && m["type"] != "progress" {
			d.last = m
			return m, nil
		}
	}
}

func (d *driver) step(role *plugin.Role, s map[string]any) error {
	switch {
	case s["hello"] != nil:
		with, _ := s["hello"].(map[string]any)
		if err := d.writeJSON(hello(role, with)); err != nil {
			return err
		}
	case s["send"] != nil:
		if err := d.writeJSON(s["send"]); err != nil {
			return err
		}
	case s["send_raw"] != nil:
		b, err := hex.DecodeString(fmt.Sprint(s["send_raw"]))
		if err != nil {
			return err
		}
		// A plugin may end on a malformed frame before reading all of it.
		_ = d.writeRaw(b)
	case s["send_arrow"] != nil:
		b, err := arrowFrame(s["send_arrow"])
		if err != nil {
			return err
		}
		if err := d.writeRaw(FrameBytes(plugin.TagArrow, b)); err != nil {
			return err
		}
	case s["close_input"] != nil:
		d.in.Close()
	case s["result_set"] != nil:
		return d.resultSet(s["result_set"].(map[string]any))
	case s["exit"] != nil:
		code, ok := d.wait(replyTimeout)
		if !ok {
			return errors.New("still running after it should have exited")
		}
		if err := match(float64(code), true, s["exit"]); err != nil {
			return fmt.Errorf("exit code: %w", err)
		}
		return nil
	default:
		return fmt.Errorf("unknown step %s", show(s))
	}
	m, ok := s["expect"]
	if !ok {
		return nil
	}
	v, err := d.reply()
	if err != nil {
		if s["or_exit"] == true {
			if _, exited := d.wait(5 * time.Second); exited {
				return nil
			}
		}
		return err
	}
	if err := match(v, true, m); err != nil {
		return fmt.Errorf("reply %s: %w", show(v), err)
	}
	return nil
}

// resultSet reads Arrow frames (at least one, with the result reply's columns), then a
// result_end whose rows are the rows sent.
func (d *driver) resultSet(spec map[string]any) error {
	columns := d.last["columns"]
	frames, rows := 0, int64(0)
	for {
		f, err := d.frame()
		if err != nil {
			return err
		}
		if f.Tag == plugin.TagArrow {
			schema, recs, err := plugin.DecodeRecords(f.Body)
			if err != nil {
				return err
			}
			names := []any{}
			for _, fl := range schema.Fields() {
				names = append(names, fl.Name)
			}
			if !equal(names, columns) {
				return fmt.Errorf("the Arrow schema has columns %v, the result reply %v", names, columns)
			}
			frames++
			for _, rec := range recs {
				rows += rec.NumRows()
				rec.Release()
			}
			continue
		}
		m, err := d.message(f)
		if err != nil {
			return err
		}
		if m["type"] == "log" || m["type"] == "progress" {
			continue
		}
		if frames == 0 {
			return fmt.Errorf("no Arrow frame (carrying the schema) before %s", show(m))
		}
		if err := match(m, true, map[string]any{"type": "result_end", "id": spec["id"]}); err != nil {
			return err
		}
		if n, _ := m["rows"].(float64); int64(n) != rows {
			return fmt.Errorf("`result_end` says %v rows, %d were sent", m["rows"], rows)
		}
		if err := match(float64(rows), true, spec["rows"]); err != nil {
			return fmt.Errorf("rows: %w", err)
		}
		return nil
	}
}

// arrowFrame is an Arrow IPC stream of `rows` null rows with the given columns.
func arrowFrame(spec any) ([]byte, error) {
	s, _ := spec.(map[string]any)
	var fields []arrow.Field
	for _, c := range asList(s["columns"]) {
		col, _ := c.(map[string]any)
		var t arrow.DataType
		switch col["type"] {
		case "int64":
			t = arrow.PrimitiveTypes.Int64
		case "float64":
			t = arrow.PrimitiveTypes.Float64
		case "utf8":
			t = arrow.BinaryTypes.String
		case "boolean":
			t = arrow.FixedWidthTypes.Boolean
		default:
			return nil, fmt.Errorf("unknown column type %v", col["type"])
		}
		fields = append(fields, arrow.Field{Name: fmt.Sprint(col["name"]), Type: t, Nullable: true})
	}
	schema := arrow.NewSchema(fields, nil)
	n, _ := s["rows"].(float64)
	cols := make([]arrow.Array, len(fields))
	for i, f := range fields {
		col, _ := asList(s["columns"])[i].(map[string]any)
		values, has := col["values"].([]any)
		if has && len(values) != int(n) {
			return nil, fmt.Errorf("`%s` has %d values for %v rows", f.Name, len(values), n)
		}
		cols[i] = column(f.Type, values, int(n))
		defer cols[i].Release()
	}
	rec := array.NewRecord(schema, cols, int64(n))
	defer rec.Release()
	return plugin.EncodeRecord(rec)
}

// column is values as an Arrow array of type t, or n nulls without values.
func column(t arrow.DataType, values []any, n int) arrow.Array {
	if values == nil {
		return array.MakeArrayOfNull(memory.DefaultAllocator, t, n)
	}
	b := array.NewBuilder(memory.DefaultAllocator, t)
	defer b.Release()
	for _, v := range values {
		switch x := v.(type) {
		case nil:
			b.AppendNull()
		case float64:
			switch bb := b.(type) {
			case *array.Int64Builder:
				bb.Append(int64(x))
			case *array.Float64Builder:
				bb.Append(x)
			default:
				b.AppendNull()
			}
		case bool:
			if bb, ok := b.(*array.BooleanBuilder); ok {
				bb.Append(x)
			} else {
				b.AppendNull()
			}
		case string:
			if bb, ok := b.(*array.StringBuilder); ok {
				bb.Append(x)
			} else {
				b.AppendNull()
			}
		}
	}
	return b.NewArray()
}

// substitute replaces every ${name}: a string that is only a placeholder becomes its value,
// whatever its type; elsewhere the value is spliced in as text.
func substitute(v any, vars map[string]any) (any, error) {
	switch x := v.(type) {
	case string:
		if strings.HasPrefix(x, "${") && strings.HasSuffix(x, "}") && strings.Count(x, "}") == 1 {
			val, ok := vars[x[2:len(x)-1]]
			if !ok {
				return nil, fmt.Errorf("unknown placeholder `%s`", x)
			}
			return val, nil
		}
		var out strings.Builder
		rest := x
		for {
			start := strings.Index(rest, "${")
			if start < 0 {
				break
			}
			end := strings.Index(rest[start:], "}")
			if end < 0 {
				return nil, fmt.Errorf("unclosed placeholder in `%s`", x)
			}
			name := rest[start+2 : start+end]
			val, ok := vars[name]
			if !ok {
				return nil, fmt.Errorf("unknown placeholder `${%s}`", name)
			}
			out.WriteString(rest[:start])
			if s, isStr := val.(string); isStr {
				out.WriteString(s)
			} else {
				out.WriteString(show(val))
			}
			rest = rest[start+end+1:]
		}
		out.WriteString(rest)
		return out.String(), nil
	case []any:
		out := make([]any, len(x))
		for i, e := range x {
			s, err := substitute(e, vars)
			if err != nil {
				return nil, err
			}
			out[i] = s
		}
		return out, nil
	case map[string]any:
		out := make(map[string]any, len(x))
		for k, e := range x {
			s, err := substitute(e, vars)
			if err != nil {
				return nil, err
			}
			out[k] = s
		}
		return out, nil
	}
	return v, nil
}

// match reports why v (present: whether it's there at all) lacks the properties m describes.
func match(v any, present bool, m any) error {
	got := func() string {
		if !present {
			return "nothing"
		}
		return show(v)
	}
	switch mm := m.(type) {
	case map[string]any:
		isOp := false
		for k := range mm {
			if strings.HasPrefix(k, "$") {
				isOp = true
			}
		}
		if isOp {
			for op, arg := range mm {
				if err := operator(v, present, op, arg); err != nil {
					return fmt.Errorf("%v (got %s)", err, got())
				}
			}
			return nil
		}
		obj, ok := v.(map[string]any)
		if !present || !ok {
			return fmt.Errorf("expected an object, got %s", got())
		}
		for k, sub := range mm {
			val, has := obj[k]
			if err := match(val, has, sub); err != nil {
				return fmt.Errorf("`%s`: %w", k, err)
			}
		}
		return nil
	case []any:
		l, ok := v.([]any)
		if !present || !ok || len(l) != len(mm) {
			return fmt.Errorf("expected %d items, got %s", len(mm), got())
		}
		for i := range mm {
			if err := match(l[i], true, mm[i]); err != nil {
				return fmt.Errorf("[%d]: %w", i, err)
			}
		}
		return nil
	}
	if !present || !same(v, m) {
		return fmt.Errorf("expected %s, got %s", show(m), got())
	}
	return nil
}

func same(a, b any) bool {
	x, okA := a.(float64)
	y, okB := b.(float64)
	if okA && okB {
		return x == y
	}
	return equal(a, b)
}

func operator(v any, present bool, op string, arg any) error {
	there := present && v != nil
	num := func() (float64, float64, error) {
		x, ok := v.(float64)
		a, okA := arg.(float64)
		if !there || !ok || !okA {
			return 0, 0, errors.New("expected a number")
		}
		return x, a, nil
	}
	switch op {
	case "$absent":
		if there != (arg == true) {
			return nil
		}
		if arg == true {
			return errors.New("expected no value")
		}
		return errors.New("expected a value")
	case "$any":
		if !present {
			return errors.New("expected a value")
		}
		return nil
	case "$one_of":
		var why []string
		for _, alt := range asList(arg) {
			err := match(v, present, alt)
			if err == nil {
				return nil
			}
			why = append(why, err.Error())
		}
		return fmt.Errorf("matches none of: %s", strings.Join(why, "; "))
	case "$type":
		if !there || !typeMatches(v, arg) {
			return fmt.Errorf("expected a %v", arg)
		}
		return nil
	case "$nonempty":
		switch x := v.(type) {
		case string:
			if x != "" {
				return nil
			}
		case []any:
			if len(x) > 0 {
				return nil
			}
		}
		return errors.New("expected a non-empty string or list")
	case "$len":
		n := -1
		switch x := v.(type) {
		case string:
			n = len([]rune(x))
		case []any:
			n = len(x)
		}
		if f, _ := arg.(float64); n < 0 || float64(n) != f {
			return fmt.Errorf("expected length %v, got %d", arg, n)
		}
		return nil
	case "$contains":
		switch x := v.(type) {
		case string:
			if s, ok := arg.(string); ok && strings.Contains(x, s) {
				return nil
			}
			return fmt.Errorf("expected text containing %s", show(arg))
		case []any:
			for _, e := range x {
				if match(e, true, arg) == nil {
					return nil
				}
			}
			return fmt.Errorf("expected an item matching %s", show(arg))
		}
		return errors.New("expected a string or list")
	case "$every":
		l, ok := v.([]any)
		if !ok {
			return errors.New("expected a list")
		}
		for i, e := range l {
			if err := match(e, true, arg); err != nil {
				return fmt.Errorf("[%d]: %w", i, err)
			}
		}
		return nil
	case "$in":
		for _, e := range asList(arg) {
			if there && same(v, e) {
				return nil
			}
		}
		return fmt.Errorf("expected one of %s", show(arg))
	case "$min":
		x, a, err := num()
		if err != nil || x < a {
			return fmt.Errorf("expected at least %v", arg)
		}
		return nil
	case "$lt":
		x, a, err := num()
		if err != nil || !(x < a) || math.IsNaN(x) {
			return fmt.Errorf("expected less than %v", arg)
		}
		return nil
	case "$file_exists":
		s, _ := v.(string)
		st, err := os.Stat(s)
		if (err == nil && st.Mode().IsRegular()) == (arg == true) {
			return nil
		}
		return fmt.Errorf("expected a path that is a file: %v", arg)
	}
	return fmt.Errorf("unknown matcher `%s`", op)
}
