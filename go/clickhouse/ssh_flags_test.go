package main

import (
	"crypto/rand"
	"crypto/rsa"
	"crypto/x509"
	"encoding/pem"
	"strings"
	"testing"
)

// A profile's env_var() reaches the plugin as text: `allow_rsa_keys: "false"` must still refuse
// an RSA key, and `use_agent: "false"` must not count as set.
func TestSSHFlagsGivenAsText(t *testing.T) {
	k, err := rsa.GenerateKey(rand.Reader, 2048)
	if err != nil {
		t.Fatal(err)
	}
	key := string(pem.EncodeToMemory(&pem.Block{Type: "RSA PRIVATE KEY", Bytes: x509.MarshalPKCS1PrivateKey(k)}))
	s := map[string]any{"host": "bastion", "username": "u", "private_key": key, "allow_rsa_keys": "false", "use_agent": "false"}
	if errs := checkSSH(s); len(errs) != 0 {
		t.Fatalf("checkSSH: %v", errs)
	}
	if _, err := (&tunnel{}).sshAuth(s); err == nil || !strings.Contains(err.Error(), "refuses RSA keys") {
		t.Fatalf("an RSA key with allow_rsa_keys \"false\": got %v", err)
	}
	s["allow_rsa_keys"] = "true"
	if _, err := (&tunnel{}).sshAuth(s); err != nil {
		t.Fatalf("allow_rsa_keys \"true\": %v", err)
	}
	s["allow_rsa_keys"] = "maybe"
	if errs := checkSSH(s); len(errs) != 1 || !strings.Contains(errs[0], "allow_rsa_keys") {
		t.Fatalf("a non-boolean allow_rsa_keys: %v", errs)
	}
}
