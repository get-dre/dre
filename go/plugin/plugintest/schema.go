package plugintest

// The published protocol schema (crates/dre-protocol/conformance/schema.json), and a checker for
// the subset of JSON Schema it uses: $ref, anyOf, oneOf, const, enum, type, properties,
// required, items, additionalProperties, minimum and pattern.

import (
	"embed"
	"encoding/json"
	"fmt"
	"math"
	"regexp"
	"slices"
	"sort"
	"strings"
	"testing"
)

// cases holds the published schema and conformance cases: a copy of
// crates/dre-protocol/conformance, embedded so the suite works wherever this module is used. A
// test in go/plugin keeps the copy in step with the original.
//
//go:embed conformance/*.json
var cases embed.FS

// CaseFile is one published file: schema.json, protocol.json or delivery.json.
func CaseFile(t testing.TB, name string) []byte {
	t.Helper()
	b, err := cases.ReadFile("conformance/" + name)
	if err != nil {
		t.Fatalf("the conformance file %s: %v", name, err)
	}
	return b
}

// SchemaDoc is the parsed schema.
type SchemaDoc struct {
	root map[string]any
}

// Protocol is the schema's version range, capabilities and error kinds.
type Protocol struct {
	MinVersion   int
	MaxVersion   int
	Capabilities []string
	ErrorKinds   []string
}

// Schema reads the published schema.
func Schema(t testing.TB) *SchemaDoc {
	t.Helper()
	var root map[string]any
	if err := json.Unmarshal(CaseFile(t, "schema.json"), &root); err != nil {
		t.Fatalf("schema.json: %v", err)
	}
	return &SchemaDoc{root: root}
}

// Protocol is the schema's `x-dre` block.
func (s *SchemaDoc) Protocol() Protocol {
	x, _ := s.root["x-dre"].(map[string]any)
	p, _ := x["protocol"].(map[string]any)
	num := func(v any) int { f, _ := v.(float64); return int(f) }
	return Protocol{
		MinVersion:   num(p["min_version"]),
		MaxVersion:   num(p["max_version"]),
		Capabilities: strs(x["capabilities"]),
		ErrorKinds:   strs(x["error_kinds"]),
	}
}

func strs(v any) []string {
	var out []string
	for _, x := range asList(v) {
		s, _ := x.(string)
		out = append(out, s)
	}
	return out
}

func asList(v any) []any { l, _ := v.([]any); return l }

// CheckResponse reports why msg (a decoded JSON message) isn't a message a plugin may send.
func (s *SchemaDoc) CheckResponse(msg any) error {
	return s.check(msg, map[string]any{"$ref": "#/$defs/Response"}, "")
}

func (s *SchemaDoc) check(v any, schema any, at string) error {
	if b, ok := schema.(bool); ok {
		if !b {
			return fmt.Errorf("%s: not allowed", where(at))
		}
		return nil
	}
	sc, _ := schema.(map[string]any)
	if ref, ok := sc["$ref"].(string); ok {
		def := s.root["$defs"].(map[string]any)[strings.TrimPrefix(ref, "#/$defs/")]
		if def == nil {
			return fmt.Errorf("%s: unknown $ref %s", where(at), ref)
		}
		if err := s.check(v, def, at); err != nil {
			return err
		}
	}
	if t, ok := sc["type"]; ok && !typeMatches(v, t) {
		return fmt.Errorf("%s: %s isn't of type %v", where(at), show(v), t)
	}
	if c, ok := sc["const"]; ok && !equal(v, c) {
		return fmt.Errorf("%s: %s isn't %v", where(at), show(v), show(c))
	}
	if e, ok := sc["enum"]; ok && !slices.ContainsFunc(asList(e), func(x any) bool { return equal(v, x) }) {
		return fmt.Errorf("%s: %s isn't one of %v", where(at), show(v), show(e))
	}
	if m, ok := sc["minimum"].(float64); ok {
		if f, isNum := v.(float64); isNum && f < m {
			return fmt.Errorf("%s: %v is below %v", where(at), f, m)
		}
	}
	if p, ok := sc["pattern"].(string); ok {
		if str, isStr := v.(string); isStr && !regexp.MustCompile(p).MatchString(str) {
			return fmt.Errorf("%s: %q doesn't match %s", where(at), str, p)
		}
	}
	if obj, isObj := v.(map[string]any); isObj {
		props, _ := sc["properties"].(map[string]any)
		for _, r := range asList(sc["required"]) {
			if _, ok := obj[r.(string)]; !ok {
				return fmt.Errorf("%s: `%s` is missing", where(at), r)
			}
		}
		keys := make([]string, 0, len(obj))
		for k := range obj {
			keys = append(keys, k)
		}
		sort.Strings(keys)
		for _, k := range keys {
			if ps, ok := props[k]; ok {
				if err := s.check(obj[k], ps, at+"."+k); err != nil {
					return err
				}
			} else if ap, ok := sc["additionalProperties"]; ok {
				if err := s.check(obj[k], ap, at+"."+k); err != nil {
					return err
				}
			}
		}
	}
	if items, ok := sc["items"]; ok {
		for i, x := range asList(v) {
			if err := s.check(x, items, fmt.Sprintf("%s[%d]", at, i)); err != nil {
				return err
			}
		}
	}
	if alts, ok := sc["anyOf"]; ok {
		if _, err := s.matching(v, asList(alts), at); err != nil {
			return err
		}
	}
	if alts, ok := sc["oneOf"]; ok {
		n, err := s.matching(v, asList(alts), at)
		if err != nil {
			return err
		}
		if n > 1 {
			return fmt.Errorf("%s: %s matches %d of oneOf", where(at), show(v), n)
		}
	}
	return nil
}

// matching is how many of alts v matches, or an error naming the closest miss when none does.
func (s *SchemaDoc) matching(v any, alts []any, at string) (int, error) {
	n := 0
	var errs []string
	for _, a := range alts {
		if err := s.check(v, a, at); err != nil {
			errs = append(errs, err.Error())
		} else {
			n++
		}
	}
	if n == 0 {
		// The alternative for this message's type says the most.
		for _, e := range errs {
			if !strings.Contains(e, ".type: ") {
				return 0, fmt.Errorf("%s", e)
			}
		}
		return 0, fmt.Errorf("%s: %s matches no alternative", where(at), show(v))
	}
	return n, nil
}

func typeMatches(v any, t any) bool {
	if l, ok := t.([]any); ok {
		return slices.ContainsFunc(l, func(x any) bool { return typeMatches(v, x) })
	}
	switch t {
	case "null":
		return v == nil
	case "boolean":
		_, ok := v.(bool)
		return ok
	case "string":
		_, ok := v.(string)
		return ok
	case "number":
		_, ok := v.(float64)
		return ok
	case "integer":
		f, ok := v.(float64)
		return ok && f == math.Trunc(f)
	case "array":
		_, ok := v.([]any)
		return ok
	case "object":
		_, ok := v.(map[string]any)
		return ok
	}
	return false
}

func equal(a, b any) bool {
	x, _ := json.Marshal(a)
	y, _ := json.Marshal(b)
	return string(x) == string(y)
}

func show(v any) string {
	b, _ := json.Marshal(v)
	return string(b)
}

func where(at string) string {
	if at == "" {
		return "message"
	}
	return "message" + at
}
