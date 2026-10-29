package plugin

// The protocol's constants and the messages a plugin sends, as typed structs. They follow the
// published schema (crates/dre-protocol/conformance/schema.json); the tests check the constants
// against it, and the conformance suite checks every message.

const (
	ProtocolMin = 0
	ProtocolMax = 1
)

// Capabilities a role may advertise.
const (
	CapSessions    = "sessions"
	CapReadOnly    = "read_only"
	CapCheck       = "check"
	CapMultiFile   = "multi_file"
	CapMessage     = "message"
	CapMessageOnly = "message_only"
	CapLoad        = "load"
	CapValidate    = "validate"
)

// Capabilities is every capability, in the schema's order.
var Capabilities = []string{CapSessions, CapReadOnly, CapCheck, CapMultiFile, CapMessage, CapMessageOnly, CapLoad, CapValidate}

// ErrorKinds is every kind an `error` reply may give, in the schema's order.
var ErrorKinds = []string{"config", "plugin", "refused", "connection", "auth", "query", "delivery", "internal", "cancelled", "timed_out"}

// envelope is what every message carries: its type and, from protocol 1, its request's id.
type envelope struct {
	Type string `json:"type"`
	ID   uint64 `json:"id,omitempty"`
}

func (e *envelope) setID(id uint64) { e.ID = id }

// message is one message a plugin sends.
type message interface{ setID(id uint64) }

func newEnvelope(t string) envelope { return envelope{Type: t} }

type helloReply struct {
	envelope
	ProtocolVersion int      `json:"protocol_version"`
	Kind            string   `json:"kind"`
	Name            string   `json:"name"`
	Version         string   `json:"version"`
	Capabilities    []string `json:"capabilities"`
	Provides        []string `json:"provides,omitempty"`
}

type versionMismatchReply struct {
	envelope
	MinVersion int `json:"min_version"`
	MaxVersion int `json:"max_version"`
}

type describeReply struct {
	envelope
	ConnectionFields []Field       `json:"connection_fields"`
	OptionFields     []OptionField `json:"option_fields,omitempty"`
	IdentifierQuote  string        `json:"identifier_quote,omitempty"`
	StringLiteral    string        `json:"string_literal,omitempty"`
}

type validatedReply struct {
	envelope
	Errors   []string `json:"errors"`
	Warnings []string `json:"warnings,omitempty"`
}

type okReply struct{ envelope }

type resultReply struct {
	envelope
	Columns []string `json:"columns"`
}

type resultEndReply struct {
	envelope
	Rows int64 `json:"rows"`
}

type noResultReply struct{ envelope }

type deliveredReply struct {
	envelope
	Location string `json:"location"`
	Attempts int    `json:"attempts,omitempty"`
}

type loadedReply struct {
	envelope
	Relation string `json:"relation"`
	Rows     int64  `json:"rows"`
	Warning  string `json:"warning,omitempty"`
}

type errorReplyMsg struct {
	envelope
	Message string `json:"message"`
	Kind    string `json:"kind,omitempty"`
	Code    string `json:"code,omitempty"`
}

type logMsg struct {
	envelope
	Level   string         `json:"level"`
	Message string         `json:"message"`
	Fields  map[string]any `json:"fields,omitempty"`
}

type progressMsg struct {
	envelope
	Message string `json:"message,omitempty"`
	Done    int64  `json:"done"`
	Total   int64  `json:"total"`
}

func okMsg() *okReply { return &okReply{newEnvelope("ok")} }

// nonNil is s, or an empty list for nil, so it's sent as [] rather than null.
func nonNil[T any](s []T) []T {
	if s == nil {
		return []T{}
	}
	return s
}
