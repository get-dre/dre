package main

import (
	"testing"

	"github.com/get-dre/dre/go/plugin/plugintest"
)

// The package against the language-neutral protocol conformance cases
// (crates/dre-protocol/conformance). The session cases need a warehouse; the plugin SDK's own
// tests run them with a fake session.
func TestConformsToTheProtocol(t *testing.T) {
	plugintest.Conformance(t, pkg, plugintest.Options{})
}
