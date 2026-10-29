package plugin_test

// The SDK against the language-neutral conformance cases (crates/dre-protocol/conformance),
// with a fake session for the session cases: the same cases every Rust plugin passes.

import (
	"testing"

	"github.com/get-dre/dre/go/plugin"
	"github.com/get-dre/dre/go/plugin/plugintest"
)

func conformancePackage() plugin.Package {
	p := pkg(nil, &[]string{})
	// A fresh session per open: a cancel case leaves its session stopped.
	p.Roles[0].Open = func(map[string]any) (plugin.Session, error) { return &fakeDB{stop: make(chan struct{})}, nil }
	return p
}

func TestTheSDKPassesTheConformanceSuite(t *testing.T) {
	plugintest.Conformance(t, conformancePackage(), plugintest.Options{Session: &plugintest.Session{
		Connection: map[string]any{},
		Statements: map[string]string{
			"rows":      "select three batches",
			"empty":     "select nothing",
			"no_result": "create table t",
			"error":     "not sql",
			"slow":      "wait",
		},
	}})
}

func TestTheSuiteFailsAPluginThatBreaksARule(t *testing.T) {
	p := conformancePackage()
	// A source without an identifier quote breaks "describe gives the identifier quote character".
	p.Roles[0].IdentifierQuote = ""
	checks := plugintest.RunConformance(t, p, plugintest.Options{})
	for _, c := range checks {
		if c.Name == "describe gives the identifier quote character" && c.Plugin == "source/fake" {
			if c.Passed {
				t.Fatal("a source without an identifier quote passed")
			}
			return
		}
	}
	t.Fatalf("the case didn't run: %v", checks)
}
