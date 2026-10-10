package plugin

// Static checks of a profile entry's connection settings, from the fields a role declares (the
// Rust SDK has the same in dre_protocol::connection): a required field missing, an unknown key,
// a value of the wrong Kind or not one of its Choices; then the role's ValidateConnection.
// Nothing here touches the network.

import (
	"fmt"
	"math"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
)

// CheckConnection checks conn against the role's Fields, then its ValidateConnection (when the
// generic checks passed). Keys in unresolved (an unset env_var()) count as set but aren't
// checked. Messages name fields, never values; secret values are redacted.
func CheckConnection(r Role, conn map[string]any, unresolved []string) (errs, warns []string) {
	errs, warns = []string{}, []string{}
	isUnresolved := func(k string) bool {
		for _, u := range unresolved {
			if u == k {
				return true
			}
		}
		return false
	}
	byName := map[string]Field{}
	var names []string
	for _, f := range r.Fields {
		byName[f.Name] = f
		names = append(names, f.Name)
	}
	accepted := map[string]bool{}
	for _, a := range r.Accepts {
		accepted[a] = true
	}
	keys := make([]string, 0, len(conn))
	for k := range conn {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	for _, k := range keys {
		v := conn[k]
		f, ok := byName[k]
		if !ok {
			if !accepted[k] {
				hint := ""
				if c := Closest(k, names); c != "" {
					hint = fmt.Sprintf("; did you mean `%s`?", c)
				}
				warns = append(warns, fmt.Sprintf("unknown key `%s` for %s `%s`, ignored%s", k, r.Kind, r.Name, hint))
			}
			continue
		}
		if v == nil || isUnresolved(k) {
			continue
		}
		if s, ok := v.(string); ok && (strings.Contains(s, "{{") || strings.Contains(s, "{%")) {
			continue
		}
		if e := checkField(f, v); e != "" {
			errs = append(errs, fmt.Sprintf("`%s` %s", k, e))
		}
	}
	for _, f := range r.Fields {
		if !f.Required || f.Default != nil {
			continue
		}
		if v, ok := conn[f.Name]; (!ok || v == nil) && !isUnresolved(f.Name) {
			errs = append(errs, fmt.Sprintf("`%s` is required", f.Name))
		}
	}
	if len(errs) == 0 && r.ValidateConnection != nil {
		errs = append(errs, r.ValidateConnection(conn)...)
	}
	return redact(errs, r.Fields, conn), redact(warns, r.Fields, conn)
}

func checkField(f Field, v any) string {
	switch f.Kind {
	case "integer":
		switch n := v.(type) {
		case float64:
			if n != math.Trunc(n) {
				return "must be a whole number"
			}
		case string:
			if _, err := strconv.ParseInt(strings.TrimSpace(n), 10, 64); err != nil {
				return "must be a whole number"
			}
		default:
			return "must be a whole number"
		}
	case "boolean":
		if _, ok := v.(bool); !ok {
			return "must be true or false"
		}
	case "duration":
		if _, err := ParseDuration(v); err != nil {
			return "must be a duration such as `30s` or `2m`, or seconds"
		}
	case "map":
		if _, ok := v.(map[string]any); !ok {
			return "must be a block of settings"
		}
	case "path":
		p, ok := v.(string)
		if !ok {
			return "must be a file path"
		}
		if strings.HasPrefix(p, "~/") {
			home, _ := os.UserHomeDir()
			p = filepath.Join(home, p[2:])
		}
		if _, err := os.Stat(p); err != nil {
			return fmt.Sprintf("names a file that doesn't exist (%s)", p)
		}
	case "string":
		switch v.(type) {
		case string, float64, bool:
		default:
			return "must be a string"
		}
	}
	if len(f.Choices) > 0 {
		s := fmt.Sprint(v)
		for _, c := range f.Choices {
			if c == s {
				return ""
			}
		}
		q := make([]string, len(f.Choices))
		for i, c := range f.Choices {
			q[i] = "`" + c + "`"
		}
		return "must be one of " + strings.Join(q, ", ")
	}
	return ""
}

// Closest is the name nearest key, when it's near enough to be a typo; "" otherwise.
func Closest(key string, names []string) string {
	best, bestD := "", math.MaxInt
	for _, n := range names {
		d := distance(key, n)
		if d <= max(2, len(n)/4) && d < bestD {
			best, bestD = n, d
		}
	}
	return best
}

func distance(a, b string) int {
	rb := []rune(b)
	row := make([]int, len(rb)+1)
	for j := range row {
		row[j] = j
	}
	for i, ca := range []rune(a) {
		prev := row[0]
		row[0] = i + 1
		for j, cb := range rb {
			cur := row[j+1]
			cost := 1
			if ca == cb {
				cost = 0
			}
			row[j+1] = min(prev+cost, row[j]+1, row[j+1]+1)
			prev = cur
		}
	}
	return row[len(rb)]
}

func redact(msgs []string, fields []Field, conn map[string]any) []string {
	for _, f := range fields {
		if !f.Secret {
			continue
		}
		s, ok := conn[f.Name].(string)
		if !ok || len(s) < 3 {
			continue
		}
		for i := range msgs {
			msgs[i] = strings.ReplaceAll(msgs[i], s, "*****")
		}
	}
	return msgs
}
