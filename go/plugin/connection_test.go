package plugin

import (
	"reflect"
	"strings"
	"testing"
)

func TestConnectionChecks(t *testing.T) {
	r := Role{Kind: "source", Name: "pg", Accepts: []string{"dbname"}, Fields: []Field{
		{Name: "host", Required: true},
		{Name: "port", Kind: "integer"},
		{Name: "password", Secret: true},
		{Name: "sslmode", Choices: []string{"disable", "require"}},
		{Name: "timeout", Kind: "duration"},
		{Name: "key_path", Kind: "path"},
	}, ValidateConnection: func(conn map[string]any) []string {
		return []string{"bad password " + conn["password"].(string)}
	}}
	errs, warns := CheckConnection(r, map[string]any{"port": "abc", "sslmode": "verify", "timeout": "soon", "hots": "x", "dbname": "d", "key_path": "/no/such"}, nil)
	if len(errs) != 5 || !strings.HasPrefix(errs[0], "`key_path` names a file") || errs[4] != "`host` is required" {
		t.Fatalf("%q", errs)
	}
	if !reflect.DeepEqual(warns, []string{"unknown key `hots` for source `pg`, ignored; did you mean `host`?"}) {
		t.Fatalf("%q", warns)
	}
	// Unresolved values count as set; the role's rules run once the generic checks pass, and
	// secrets are redacted.
	errs, _ = CheckConnection(r, map[string]any{"host": nil, "password": "hunter22"}, []string{"host"})
	if !reflect.DeepEqual(errs, []string{"bad password *****"}) {
		t.Fatalf("%q", errs)
	}
}
