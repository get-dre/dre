package plugin_test

// The SDK's protocol constants against the published schema (crates/dre-protocol/conformance),
// which dre-protocol generates from its message types: the schema is the source of truth.

import (
	"bytes"
	"os"
	"path/filepath"
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

// The copy plugintest embeds is the published one, so the suite works outside this repository.
func TestTheEmbeddedCasesAreThePublishedOnes(t *testing.T) {
	for _, name := range []string{"schema.json", "protocol.json", "delivery.json"} {
		published, err := os.ReadFile(filepath.Join("..", "..", "crates", "dre-protocol", "conformance", name))
		if err != nil {
			t.Fatal(err)
		}
		if !bytes.Equal(published, plugintest.CaseFile(t, name)) {
			t.Errorf("plugintest/conformance/%s differs from crates/dre-protocol/conformance/%s; "+
				"update it with `DRE_UPDATE_SCHEMA=1 cargo test -p dre-protocol --lib schema`", name, name)
		}
	}
}
