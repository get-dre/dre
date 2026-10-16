package main

// The `ssh:` block: reach ClickHouse through an SSH bastion. The settings and rules are the
// `postgres` source's (the dre-ssh crate): `host`, `port` (22), `username`, and `password`,
// `private_key_path`, `private_key` (with `private_key_passphrase`) or `use_agent: true`. The
// bastion's host key must be trusted: it's checked against `known_hosts_path` (default
// ~/.ssh/known_hosts) or a pinned `host_key_fingerprint` (`SHA256:...`, as `ssh-keygen -lf`
// prints it).

import (
	"context"
	"crypto/tls"
	"errors"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"runtime"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"

	"golang.org/x/crypto/ssh"
	"golang.org/x/crypto/ssh/agent"
	"golang.org/x/crypto/ssh/knownhosts"

	"github.com/get-dre/dre/go/plugin"
)

var sshKeys = []string{
	"host", "port", "username", "password", "private_key_path", "private_key", "private_key_passphrase",
	"known_hosts_path", "host_key_fingerprint", "use_agent", "allow_rsa_keys",
}

// checkSSH is the static check of an `ssh:` block: no network, no files read.
func checkSSH(s map[string]any) []string {
	var errs []string
	for _, k := range sortedKeys(s) {
		if !contains(sshKeys, k) {
			hint := ""
			if c := plugin.Closest(k, sshKeys); c != "" {
				hint = fmt.Sprintf("; did you mean `ssh.%s`?", c)
			}
			errs = append(errs, fmt.Sprintf("unknown key `ssh.%s`%s", k, hint))
		}
	}
	for _, k := range []string{"host", "username"} {
		if plugin.Optional(s, k) == "" {
			errs = append(errs, fmt.Sprintf("`ssh.%s` is required", k))
		}
	}
	agentOn, _ := s["use_agent"].(bool)
	keyPath, keyText := plugin.Optional(s, "private_key_path") != "", plugin.Optional(s, "private_key") != ""
	switch {
	case keyPath && keyText:
		errs = append(errs, "set `ssh.private_key_path` or `ssh.private_key`, not both")
	case agentOn && (keyPath || keyText):
		errs = append(errs, "set `ssh.use_agent` or a private key (`ssh.private_key_path`, `ssh.private_key`), not both")
	case !agentOn && !keyPath && !keyText && plugin.Optional(s, "password") == "":
		errs = append(errs, "set `ssh.password`, `ssh.private_key_path`, `ssh.private_key` or `ssh.use_agent: true`")
	}
	return errs
}

// tunnel is an SSH connection to the bastion, opened on first use; ClickHouse connections run
// over its channels.
type tunnel struct {
	settings map[string]any
	timeout  time.Duration
	once     sync.Once
	client   *ssh.Client
	agent    net.Conn // the SSH agent's socket, with use_agent
	err      error
}

// dialer is a DialContext for the driver: every connection to addr goes through the bastion,
// wrapped in TLS when tlsConf is set (the driver doesn't add TLS over a dialer of its own on the
// native protocol; over HTTP, net/http does).
func (t *tunnel) dialer(tlsConf *tls.Config) func(ctx context.Context, addr string) (net.Conn, error) {
	return func(ctx context.Context, addr string) (net.Conn, error) {
		t.once.Do(func() { t.client, t.err = t.connect() })
		if t.err != nil {
			return nil, t.err
		}
		c, err := t.client.DialContext(ctx, "tcp", addr)
		if err != nil {
			return nil, fmt.Errorf("the SSH bastion can't reach %s: %v", addr, err)
		}
		if tlsConf == nil {
			return c, nil
		}
		conf := tlsConf.Clone()
		if conf.ServerName == "" {
			conf.ServerName, _, _ = net.SplitHostPort(addr)
		}
		tc := tls.Client(c, conf)
		if err := tc.HandshakeContext(ctx); err != nil {
			c.Close()
			return nil, err
		}
		return tc, nil
	}
}

func (t *tunnel) closeIfSet() {
	if t != nil && t.agent != nil {
		t.agent.Close()
	}
	if t != nil && t.client != nil {
		t.client.Close()
	}
}

func (t *tunnel) connect() (*ssh.Client, error) {
	s := t.settings
	if errs := checkSSH(s); len(errs) > 0 {
		return nil, errors.New(strings.Join(errs, "; "))
	}
	host := plugin.Optional(s, "host")
	port, err := plugin.Int(s, "port", 22)
	if err != nil {
		return nil, fmt.Errorf("invalid `ssh.port`")
	}
	addr := net.JoinHostPort(host, strconv.FormatInt(port, 10))
	auth, err := t.sshAuth(s)
	if err != nil {
		return nil, err
	}
	hostKey, err := hostKeyCheck(s)
	if err != nil {
		return nil, err
	}
	var presented ssh.PublicKey
	check := func(h string, a net.Addr, k ssh.PublicKey) error { presented = k; return hostKey(h, a, k) }
	conf := &ssh.ClientConfig{User: plugin.Optional(s, "username"), Auth: auth, HostKeyCallback: check, Timeout: t.timeout}
	c, err := ssh.Dial("tcp", addr, conf)
	if err != nil {
		var ke *knownhosts.KeyError
		switch {
		case errors.As(err, &ke) && len(ke.Want) == 0:
			return nil, fmt.Errorf("the SSH bastion %s isn't in known_hosts; its host key is %s: pin it as `ssh.host_key_fingerprint` once you've checked it, or add the host to known_hosts", addr, ssh.FingerprintSHA256(presented))
		case errors.As(err, &ke):
			return nil, fmt.Errorf("the SSH bastion %s presented a host key that doesn't match known_hosts: refusing to connect", addr)
		case strings.Contains(err.Error(), "unable to authenticate"):
			return nil, fmt.Errorf("the SSH bastion %s refused the credentials for `%s`", addr, conf.User)
		}
		return nil, fmt.Errorf("can't connect to the SSH bastion %s: %v", addr, err)
	}
	return c, nil
}

func (t *tunnel) sshAuth(s map[string]any) ([]ssh.AuthMethod, error) {
	if on, _ := s["use_agent"].(bool); on {
		c, err := agentConn()
		if err != nil {
			return nil, err
		}
		t.agent = c
		return []ssh.AuthMethod{ssh.PublicKeysCallback(agent.NewClient(c).Signers)}, nil
	}
	var pem []byte
	switch {
	case plugin.Optional(s, "private_key_path") != "":
		path := plugin.Optional(s, "private_key_path")
		b, err := os.ReadFile(expandHome(path))
		if err != nil {
			return nil, fmt.Errorf("can't load private key %s: %v", path, err)
		}
		pem = b
	case plugin.Optional(s, "private_key") != "":
		pem = []byte(keyText(plugin.Optional(s, "private_key")))
	default:
		return []ssh.AuthMethod{ssh.Password(plugin.Optional(s, "password"))}, nil
	}
	var signer ssh.Signer
	var err error
	if pass := plugin.Optional(s, "private_key_passphrase"); pass != "" {
		signer, err = ssh.ParsePrivateKeyWithPassphrase(pem, []byte(pass))
	} else {
		signer, err = ssh.ParsePrivateKey(pem)
	}
	if err != nil {
		return nil, fmt.Errorf("can't read the SSH private key: %v", err)
	}
	if allow, ok := s["allow_rsa_keys"].(bool); ok && !allow && signer.PublicKey().Type() == ssh.KeyAlgoRSA {
		return nil, errors.New("the SSH private key is an RSA key, and `ssh.allow_rsa_keys: false` refuses RSA keys")
	}
	return []ssh.AuthMethod{ssh.PublicKeys(signer)}, nil
}

func agentConn() (net.Conn, error) {
	if runtime.GOOS == "windows" {
		return nil, errors.New("`ssh.use_agent` isn't supported on Windows by the clickhouse plugin; use a key file")
	}
	sock := os.Getenv("SSH_AUTH_SOCK")
	if sock == "" {
		return nil, errors.New("can't reach the SSH agent (`ssh.use_agent: true`): SSH_AUTH_SOCK isn't set")
	}
	c, err := net.Dial("unix", sock)
	if err != nil {
		return nil, fmt.Errorf("can't reach the SSH agent (`ssh.use_agent: true`): %v", err)
	}
	return c, nil
}

// hostKeyCheck trusts a pinned fingerprint, or else known_hosts.
func hostKeyCheck(s map[string]any) (ssh.HostKeyCallback, error) {
	if fp := plugin.Optional(s, "host_key_fingerprint"); fp != "" {
		return func(_ string, _ net.Addr, key ssh.PublicKey) error {
			if got := ssh.FingerprintSHA256(key); got != fp {
				return fmt.Errorf("the SSH bastion's host key is %s, not the pinned `ssh.host_key_fingerprint`: refusing to connect", got)
			}
			return nil
		}, nil
	}
	path := plugin.Optional(s, "known_hosts_path")
	if path == "" {
		home, _ := os.UserHomeDir()
		path = filepath.Join(home, ".ssh", "known_hosts")
	}
	if _, err := os.Stat(expandHome(path)); errors.Is(err, os.ErrNotExist) {
		// No known_hosts yet: every host is unknown.
		return func(string, net.Addr, ssh.PublicKey) error { return &knownhosts.KeyError{} }, nil
	}
	cb, err := knownhosts.New(expandHome(path))
	if err != nil {
		return nil, fmt.Errorf("can't read known_hosts %s (or pin `ssh.host_key_fingerprint`): %v", path, err)
	}
	return cb, nil
}

// expandHome reads a leading `~/` as the home directory (profiles aren't run through a shell).
func expandHome(p string) string {
	if strings.HasPrefix(p, "~/") || strings.HasPrefix(p, `~\`) {
		home, _ := os.UserHomeDir()
		return filepath.Join(home, p[2:])
	}
	return p
}

// keyText gives a private key stored on one line with literal `\n` its line breaks back.
func keyText(s string) string {
	if strings.Contains(s, "\n") {
		return s
	}
	return strings.ReplaceAll(strings.ReplaceAll(s, `\r\n`, "\n"), `\n`, "\n")
}

func contains(list []string, s string) bool {
	for _, x := range list {
		if x == s {
			return true
		}
	}
	return false
}

func sortedKeys(m map[string]any) []string {
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	return keys
}
