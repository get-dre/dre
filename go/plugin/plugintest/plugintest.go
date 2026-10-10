// Package plugintest drives a Go plugin's protocol loop over in-memory pipes, as core would.
package plugintest

import (
	"bufio"
	"encoding/binary"
	"encoding/json"
	"io"
	"strings"
	"testing"

	"github.com/apache/arrow-go/v18/arrow"

	"github.com/get-dre/dre/go/plugin"
)

// Conversation is one plugin process: requests in, replies out.
type Conversation struct {
	t    *testing.T
	in   *io.PipeWriter
	out  *bufio.Reader
	Code chan int
	// Logs holds the `log` and `progress` messages Reply skipped, in order.
	Logs []map[string]any
}

// Start serves p as role.
func Start(t *testing.T, p plugin.Package, role plugin.Role) *Conversation {
	inR, inW := io.Pipe()
	outR, outW := io.Pipe()
	c := &Conversation{t: t, in: inW, out: bufio.NewReader(outR), Code: make(chan int, 1)}
	go func() {
		c.Code <- plugin.Serve(inR, outW, p, role)
		outW.Close()
	}()
	return c
}

// SendRaw writes bytes as they are.
func (c *Conversation) SendRaw(b []byte) {
	if _, err := c.in.Write(b); err != nil {
		c.t.Fatal(err)
	}
}

// Send writes one JSON request.
func (c *Conversation) Send(v any) {
	b, _ := json.Marshal(v)
	c.SendRaw(FrameBytes(plugin.TagJSON, b))
}

// SendRecord writes one Arrow frame.
func (c *Conversation) SendRecord(rec arrow.Record) {
	b, err := plugin.EncodeRecord(rec)
	if err != nil {
		c.t.Fatal(err)
	}
	c.SendRaw(FrameBytes(plugin.TagArrow, b))
}

// CloseInput closes the plugin's stdin.
func (c *Conversation) CloseInput() { c.in.Close() }

// FrameBytes is one frame on the wire.
func FrameBytes(tag byte, body []byte) []byte {
	out := make([]byte, 5, 5+len(body))
	binary.BigEndian.PutUint32(out, uint32(len(body)+1))
	out[4] = tag
	return append(out, body...)
}

// Reply reads one JSON reply, keeping any `log` and `progress` messages before it in Logs.
func (c *Conversation) Reply() map[string]any {
	c.t.Helper()
	for {
		m := c.Message()
		if t := m["type"]; t != "log" && t != "progress" {
			return m
		}
		c.Logs = append(c.Logs, m)
	}
}

// Message reads the next JSON message, whatever it is.
func (c *Conversation) Message() map[string]any {
	c.t.Helper()
	f, err := plugin.ReadFrame(c.out)
	if err != nil {
		c.t.Fatalf("reading a reply: %v", err)
	}
	if f.Tag != plugin.TagJSON {
		c.t.Fatalf("expected a JSON reply, got an Arrow frame")
	}
	var m map[string]any
	json.Unmarshal(f.Body, &m)
	return m
}

// Records reads one Arrow frame.
func (c *Conversation) Records() []arrow.Record {
	c.t.Helper()
	f, err := plugin.ReadFrame(c.out)
	if err != nil || f.Tag != plugin.TagArrow {
		c.t.Fatalf("expected an Arrow frame (%v)", err)
	}
	_, recs, err := plugin.DecodeRecords(f.Body)
	if err != nil {
		c.t.Fatal(err)
	}
	return recs
}

// Hello greets the plugin and returns its hello reply.
func (c *Conversation) Hello() map[string]any {
	c.t.Helper()
	c.Send(map[string]any{"type": "hello", "min_version": 0, "max_version": 3, "core_version": "test"})
	r := c.Reply()
	if r["type"] != "hello" || r["protocol_version"] != float64(plugin.ProtocolMax) {
		c.t.Fatalf("hello: %v", r)
	}
	return r
}

// Open opens a session with conn and expects ok.
func (c *Conversation) Open(conn map[string]any) {
	c.t.Helper()
	c.Send(map[string]any{"type": "open", "connection": conn, "read_only": false})
	if r := c.Reply(); r["type"] != "ok" {
		c.t.Fatalf("open: %v", r)
	}
}

// Query runs sql and returns the result's records, after checking the reply sequence.
func (c *Conversation) Query(sql string) []arrow.Record {
	c.t.Helper()
	c.Send(map[string]any{"type": "execute", "sql": sql})
	if r := c.Reply(); r["type"] != "result" {
		c.t.Fatalf("execute %q: %v", sql, r)
	}
	var recs []arrow.Record
	for {
		f, err := plugin.ReadFrame(c.out)
		if err != nil {
			c.t.Fatal(err)
		}
		if f.Tag == plugin.TagArrow {
			_, more, err := plugin.DecodeRecords(f.Body)
			if err != nil {
				c.t.Fatal(err)
			}
			recs = append(recs, more...)
			continue
		}
		var m map[string]any
		json.Unmarshal(f.Body, &m)
		if m["type"] != "result_end" {
			c.t.Fatalf("expected result_end, got %v", m)
		}
		return recs
	}
}

// FieldNames is a describe reply's connection field names, comma-separated.
func FieldNames(describe map[string]any) string {
	var names []string
	for _, f := range describe["connection_fields"].([]any) {
		names = append(names, f.(map[string]any)["name"].(string))
	}
	return strings.Join(names, ",")
}

// ExpectError fails unless r is an error containing contains.
func ExpectError(t *testing.T, r map[string]any, contains string) {
	t.Helper()
	if r["type"] != "error" || !strings.Contains(r["message"].(string), contains) {
		t.Fatalf("expected an error containing %q, got %v", contains, r)
	}
}

// Exec runs sql and discards whatever it returns (no_result, or a result set), failing on an
// error. It returns the first reply's type.
func (c *Conversation) Exec(sql string) string {
	c.t.Helper()
	c.Send(map[string]any{"type": "execute", "sql": sql})
	r := c.Reply()
	switch r["type"] {
	case "no_result":
		return "no_result"
	case "result":
		for {
			f, err := plugin.ReadFrame(c.out)
			if err != nil {
				c.t.Fatal(err)
			}
			if f.Tag == plugin.TagJSON {
				var m map[string]any
				json.Unmarshal(f.Body, &m)
				if m["type"] != "result_end" {
					c.t.Fatalf("execute %q: %v", sql, m)
				}
				return "result"
			}
		}
	}
	c.t.Fatalf("execute %q: %v", sql, r)
	return ""
}
