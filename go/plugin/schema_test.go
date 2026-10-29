package plugin_test

// The SDK's protocol constants against the published schema (crates/dre-protocol/conformance),
// which dre-protocol generates from its message types: the schema is the source of truth.

import (
	"slices"
	"testing"

	"github.com/get-dre/dre/go/plugin"
	"github.com/get-dre/dre/go/plugin/plugintest"
)

func TestConstantsMatchTheSchema(t *testing.T) {
	x := plugintest.Schema(t).Protocol()
	if x.MinVersion != plugin.ProtocolMin || x.MaxVersion != plugin.ProtocolMax {
		t.Fatalf("the SDK speaks %d..%d, the schema %d..%d", plugin.ProtocolMin, plugin.ProtocolMax, x.MinVersion, x.MaxVersion)
	}
	if !slices.Equal(x.Capabilities, plugin.Capabilities) {
		t.Fatalf("capabilities: SDK %v, schema %v", plugin.Capabilities, x.Capabilities)
	}
	if !slices.Equal(x.ErrorKinds, plugin.ErrorKinds) {
		t.Fatalf("error kinds: SDK %v, schema %v", plugin.ErrorKinds, x.ErrorKinds)
	}
}
