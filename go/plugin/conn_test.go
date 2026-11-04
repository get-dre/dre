package plugin

import (
	"reflect"
	"testing"
)

func TestStringFields(t *testing.T) {
	for _, c := range []struct {
		name          string
		conn          map[string]any
		want, wantErr string
	}{
		{"missing", nil, "", "the profile output needs a `host` field"},
		{"empty", map[string]any{"host": ""}, "", "the profile output needs a `host` field"},
		{"null", map[string]any{"host": nil}, "", "the profile output needs a `host` field"},
		{"wrong type", map[string]any{"host": []any{"x"}}, "", "the profile output needs a `host` field"},
		{"text", map[string]any{"host": "warehouse"}, "warehouse", ""},
		{"number", map[string]any{"host": 123.5}, "123.5", ""},
		{"boolean", map[string]any{"host": false}, "false", ""},
	} {
		t.Run(c.name, func(t *testing.T) {
			if got := Optional(c.conn, "host"); got != c.want {
				t.Errorf("Optional = %q, want %q", got, c.want)
			}
			got, err := Required(c.conn, "host")
			if got != c.want || errorText(err) != c.wantErr {
				t.Fatalf("Required = %q, %v; want %q, %q", got, err, c.want, c.wantErr)
			}
		})
	}
}

func TestIntegerFields(t *testing.T) {
	for _, c := range []struct {
		name    string
		conn    map[string]any
		want    int64
		wantErr string
	}{
		{"missing", nil, 42, ""}, {"null", map[string]any{"port": nil}, 42, ""}, {"empty", map[string]any{"port": ""}, 42, ""},
		{"zero", map[string]any{"port": 0.0}, 0, ""}, {"number", map[string]any{"port": 8123.0}, 8123, ""},
		{"text", map[string]any{"port": " -22 "}, -22, ""},
		{"fraction", map[string]any{"port": 1.5}, 0, "`port` must be a whole number, got 1.5"},
		{"boolean", map[string]any{"port": true}, 0, "`port` must be a whole number, got true"},
		{"invalid", map[string]any{"port": "abc"}, 0, "`port` must be a whole number, got abc"},
		{"overflow", map[string]any{"port": "9223372036854775808"}, 0, "`port` must be a whole number, got 9223372036854775808"},
	} {
		t.Run(c.name, func(t *testing.T) {
			got, err := Int(c.conn, "port", 42)
			if got != c.want || errorText(err) != c.wantErr {
				t.Fatalf("got %d, %v; want %d, %q", got, err, c.want, c.wantErr)
			}
		})
	}
}

func TestBooleanFields(t *testing.T) {
	for _, c := range []struct {
		name      string
		conn      map[string]any
		def, want bool
		wantErr   string
	}{
		{"missing", nil, true, true, ""}, {"null", map[string]any{"secure": nil}, false, false, ""},
		{"empty", map[string]any{"secure": ""}, true, true, ""},
		{"boolean", map[string]any{"secure": false}, true, false, ""},
		{"true", map[string]any{"secure": " TRUE "}, false, true, ""},
		{"yes", map[string]any{"secure": "yes"}, false, true, ""}, {"one", map[string]any{"secure": "1"}, false, true, ""},
		{"false", map[string]any{"secure": "false"}, true, false, ""},
		{"no", map[string]any{"secure": " NO "}, true, false, ""}, {"zero", map[string]any{"secure": "0"}, true, false, ""},
		{"number", map[string]any{"secure": 1.0}, false, false, "`secure` must be true or false, got 1"},
		{"invalid", map[string]any{"secure": "perhaps"}, false, false, "`secure` must be true or false, got perhaps"},
	} {
		t.Run(c.name, func(t *testing.T) {
			got, err := Bool(c.conn, "secure", c.def)
			if got != c.want || errorText(err) != c.wantErr {
				t.Fatalf("got %v, %v; want %v, %q", got, err, c.want, c.wantErr)
			}
		})
	}
}

func TestStringLists(t *testing.T) {
	for _, c := range []struct {
		name  string
		value any
		want  []string
	}{
		{"missing", nil, nil}, {"invalid", true, nil}, {"comma separated", " read, ,write ", []string{"read", "write"}},
		{"JSON list", []any{"read", nil, 3.0, "", "write"}, []string{"read", "write"}},
	} {
		t.Run(c.name, func(t *testing.T) {
			if got := Strings(map[string]any{"scopes": c.value}, "scopes"); !reflect.DeepEqual(got, c.want) {
				t.Fatalf("got %v, want %v", got, c.want)
			}
		})
	}
}

func TestUnknownFields(t *testing.T) {
	for _, c := range []struct {
		name    string
		conn    map[string]any
		wantErr string
	}{
		{"empty", nil, ""}, {"known and extra", map[string]any{"host": "x", "threads": 4.0}, ""},
		{"sorted errors", map[string]any{"zebra": true, "host": "x", "alpha": true}, "unknown profile field(s) `alpha`, `zebra`; check the spelling"},
	} {
		t.Run(c.name, func(t *testing.T) {
			if err := Unknown(c.conn, []Field{{Name: "host"}}, "threads"); errorText(err) != c.wantErr {
				t.Fatalf("got %v, want %q", err, c.wantErr)
			}
		})
	}
}

func errorText(err error) string {
	if err != nil {
		return err.Error()
	}
	return ""
}
