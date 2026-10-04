package plugin

import (
	"fmt"
	"sort"
	"strconv"
	"strings"
)

// Helpers for a profile target's fields, as core sends them in `open`: YAML values arrive as
// JSON strings, numbers and booleans (env_var() values always as strings).

// Required is a string field that must be set.
func Required(conn map[string]any, key string) (string, error) {
	if v := Optional(conn, key); v != "" {
		return v, nil
	}
	return "", fmt.Errorf("the profile output needs a `%s` field", key)
}

// Optional is a string field, or "" when it isn't set. Numbers are given as their text.
func Optional(conn map[string]any, key string) string {
	switch v := conn[key].(type) {
	case string:
		return v
	case float64:
		return strconv.FormatFloat(v, 'f', -1, 64)
	case bool:
		return strconv.FormatBool(v)
	}
	return ""
}

// Number is an integer field given as a number or a string.
func Number(v any) (int, bool) {
	switch n := v.(type) {
	case float64:
		return int(n), true
	case string:
		i, err := strconv.Atoi(strings.TrimSpace(n))
		return i, err == nil
	}
	return 0, false
}

// Int is an integer field, def when it isn't set; a value that isn't a number is an error.
func Int(conn map[string]any, key string, def int64) (int64, error) {
	v, ok := conn[key]
	if !ok || v == nil || v == "" {
		return def, nil
	}
	switch n := v.(type) {
	case float64:
		if n == float64(int64(n)) {
			return int64(n), nil
		}
	case string:
		if i, err := strconv.ParseInt(strings.TrimSpace(n), 10, 64); err == nil {
			return i, nil
		}
	}
	return 0, fmt.Errorf("`%s` must be a whole number, got %v", key, v)
}

// Bool is a boolean field (true/false, or yes/no/1/0 as text), def when it isn't set.
func Bool(conn map[string]any, key string, def bool) (bool, error) {
	v, ok := conn[key]
	if !ok || v == nil || v == "" {
		return def, nil
	}
	switch b := v.(type) {
	case bool:
		return b, nil
	case string:
		switch strings.ToLower(strings.TrimSpace(b)) {
		case "true", "yes", "1":
			return true, nil
		case "false", "no", "0":
			return false, nil
		}
	}
	return false, fmt.Errorf("`%s` must be true or false, got %v", key, v)
}

// Strings is a field holding a list of strings, or one string (comma-separated values are
// split).
func Strings(conn map[string]any, key string) []string {
	switch v := conn[key].(type) {
	case string:
		var out []string
		for _, s := range strings.Split(v, ",") {
			if s = strings.TrimSpace(s); s != "" {
				out = append(out, s)
			}
		}
		return out
	case []any:
		var out []string
		for _, s := range v {
			if t, ok := s.(string); ok && t != "" {
				out = append(out, t)
			}
		}
		return out
	}
	return nil
}

// Unknown names the fields of conn that aren't in fields (nor in also), so a misspelt profile
// field is an error rather than silently ignored.
func Unknown(conn map[string]any, fields []Field, also ...string) error {
	known := map[string]bool{}
	for _, f := range fields {
		known[f.Name] = true
	}
	for _, a := range also {
		known[a] = true
	}
	var bad []string
	for k := range conn {
		if !known[k] {
			bad = append(bad, "`"+k+"`")
		}
	}
	if len(bad) == 0 {
		return nil
	}
	sort.Strings(bad)
	return fmt.Errorf("unknown profile field(s) %s; check the spelling", strings.Join(bad, ", "))
}
