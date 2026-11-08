package main

import (
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"errors"
	"net"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

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

// A failed bastion connection is tried again on the next dial, not kept for the whole run.
func TestTheTunnelReconnectsAfterAFailure(t *testing.T) {
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer ln.Close()
	accepted := make(chan struct{}, 4)
	go func() {
		for {
			c, err := ln.Accept()
			if err != nil {
				return
			}
			accepted <- struct{}{}
			c.Close() // the bastion drops the connection before the handshake
		}
	}()
	_, port, _ := net.SplitHostPort(ln.Addr().String())
	tun := &tunnel{settings: map[string]any{"host": "127.0.0.1", "port": port, "username": "u", "password": "p",
		"host_key_fingerprint": "SHA256:x"}, timeout: 5 * time.Second}
	dial := tun.dialer(nil)
	for range 2 {
		if _, err := dial(context.Background(), "clickhouse:8123"); err == nil {
			t.Fatal("dialed through a bastion that drops every connection")
		}
	}
	if len(accepted) != 2 {
		t.Fatalf("the bastion was reached %d times, want 2", len(accepted))
	}
}

// silentBastion accepts TCP but never sends the SSH identification; closing it releases the
// connections it holds.
func silentBastion(t *testing.T) (port string, accepted <-chan struct{}) {
	t.Helper()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	ch := make(chan struct{}, 4)
	conns := make(chan net.Conn, 4)
	t.Cleanup(func() {
		ln.Close()
		for len(conns) > 0 {
			(<-conns).Close()
		}
	})
	go func() {
		for {
			c, err := ln.Accept()
			if err != nil {
				return
			}
			conns <- c
			ch <- struct{}{}
		}
	}()
	_, port, _ = net.SplitHostPort(ln.Addr().String())
	return port, ch
}

func silentTunnel(port string, timeout time.Duration) *tunnel {
	return &tunnel{settings: map[string]any{"host": "127.0.0.1", "port": port, "username": "u", "password": "p",
		"host_key_fingerprint": "SHA256:x"}, timeout: timeout}
}

// A bastion that accepts TCP but stays silent is given up on after `connect_timeout`.
func TestASilentBastionHandshakeTimesOut(t *testing.T) {
	port, _ := silentBastion(t)
	done := make(chan error, 1)
	go func() {
		_, err := silentTunnel(port, 50*time.Millisecond).dialer(nil)(context.Background(), "clickhouse:8123")
		done <- err
	}()
	select {
	case err := <-done:
		if err == nil {
			t.Fatal("dialed through a silent bastion")
		}
	case <-time.After(10 * time.Second):
		t.Fatal("the SSH handshake ignored connect_timeout")
	}
}

// Cancelling the dial's context interrupts a handshake the bastion never answers.
func TestCancellingInterruptsASilentBastionHandshake(t *testing.T) {
	port, accepted := silentBastion(t)
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() {
		_, err := silentTunnel(port, time.Hour).dialer(nil)(ctx, "clickhouse:8123")
		done <- err
	}()
	<-accepted // the handshake has started
	cancel()
	select {
	case err := <-done:
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("got %v, want context.Canceled", err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("cancelling didn't interrupt the SSH handshake")
	}
}
