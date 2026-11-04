package main

import (
	"crypto/ed25519"
	"crypto/rand"
	"errors"
	"net"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"golang.org/x/crypto/ssh"
	"golang.org/x/crypto/ssh/knownhosts"
)

func TestHostKeyCheck(t *testing.T) {
	key := func() ssh.PublicKey {
		t.Helper()
		pub, _, err := ed25519.GenerateKey(rand.Reader)
		if err != nil {
			t.Fatal(err)
		}
		k, err := ssh.NewPublicKey(pub)
		if err != nil {
			t.Fatal(err)
		}
		return k
	}
	trusted, other := key(), key()
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("USERPROFILE", home)
	if err := os.Mkdir(filepath.Join(home, ".ssh"), 0o700); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(home, ".ssh", "known_hosts")
	if err := os.WriteFile(path, []byte(knownhosts.Line([]string{"bastion.example"}, trusted)+"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	malformed := filepath.Join(home, "malformed")
	if err := os.WriteFile(malformed, []byte("bastion.example ssh-ed25519 not-base64\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	remote := &net.TCPAddr{IP: net.ParseIP("192.0.2.1"), Port: 22}
	for _, c := range []struct {
		name     string
		conn     map[string]any
		host     string
		key      ssh.PublicKey
		wantErr  string
		keyError bool
	}{
		{"matching pin", map[string]any{"host_key_fingerprint": ssh.FingerprintSHA256(trusted)}, "bastion.example:22", trusted, "", false},
		{"mismatched pin", map[string]any{"host_key_fingerprint": ssh.FingerprintSHA256(trusted)}, "bastion.example:22", other, "not the pinned `ssh.host_key_fingerprint`: refusing to connect", false},
		{"malformed pin fails closed", map[string]any{"host_key_fingerprint": "invalid"}, "bastion.example:22", trusted, "refusing to connect", false},
		{"pin takes precedence", map[string]any{"host_key_fingerprint": ssh.FingerprintSHA256(trusted), "known_hosts_path": malformed}, "bastion.example:22", trusted, "", false},
		{"known host", map[string]any{"known_hosts_path": path}, "bastion.example:22", trusted, "", false},
		{"changed key", map[string]any{"known_hosts_path": path}, "bastion.example:22", other, "knownhosts: key mismatch", true},
		{"unknown host", map[string]any{"known_hosts_path": path}, "unknown.example:22", trusted, "knownhosts: key is unknown", true},
		{"missing file", map[string]any{"known_hosts_path": filepath.Join(home, "missing")}, "bastion.example:22", trusted, "knownhosts: key is unknown", true},
		{"default file", nil, "bastion.example:22", trusted, "", false},
		{"expanded home", map[string]any{"known_hosts_path": "~/.ssh/known_hosts"}, "bastion.example:22", trusted, "", false},
	} {
		t.Run(c.name, func(t *testing.T) {
			cb, err := hostKeyCheck(c.conn)
			if err != nil {
				t.Fatal(err)
			}
			err = cb(c.host, remote, c.key)
			if c.wantErr == "" {
				if err != nil {
					t.Fatal(err)
				}
				return
			}
			if err == nil || !strings.Contains(err.Error(), c.wantErr) {
				t.Fatalf("got %v, want %q", err, c.wantErr)
			}
			if c.keyError {
				var ke *knownhosts.KeyError
				if !errors.As(err, &ke) {
					t.Fatalf("expected KeyError, got %T", err)
				}
			}
		})
	}
	if cb, err := hostKeyCheck(map[string]any{"known_hosts_path": malformed}); err == nil || cb != nil || !strings.Contains(err.Error(), "can't read known_hosts "+malformed) {
		t.Fatalf("malformed known_hosts: %v", err)
	}
}
