package main

// The databricks destination: a Unity Catalog Volume or a workspace file, chosen by the path.
// Profile target fields: host plus the source's sign-in fields (auth_type, token, client_id,
// client_secret), so one set of credentials, and one OAuth session per workspace, serves both.

import (
	"errors"
	"fmt"
	"io"
	"io/fs"
	"net/http"
	"os"
	"path/filepath"
	"strings"

	"github.com/get-dre/dre/go/plugin"
)

// deliveryOptions: `if_exists` (the shared delivery rules). Volumes and workspace files appear
// only once uploaded, so there's no temporary name.
func deliveryOptions() []plugin.OptionField {
	return plugin.DeliveryOptions()[:1]
}

// rulesFor is the delivery rules from a destination entry's options.
func rulesFor(opts map[string]any) (plugin.Rules, error) {
	r, _, err := plugin.RulesFrom(plugin.DefaultRules(), nil, opts, nil)
	return r, err
}

// failed words a delivery failure; a coded *plugin.Error (`if_exists: error`) passes as it is.
func failed(what, path string, err error) error {
	var pe *plugin.Error
	if errors.As(err, &pe) {
		return err
	}
	return fmt.Errorf("%s %s failed: %v; the output is still in target/", what, path, err)
}

// mountStore writes to /Volumes or /Workspace where Databricks compute mounts them; root stands
// in for /. Files are written in place, as before the delivery rules.
type mountStore struct{ root string }

func (s mountStore) path(p string) string {
	return filepath.Join(s.root, filepath.FromSlash(strings.TrimPrefix(p, "/")))
}

func (mountStore) Caps() plugin.Caps {
	return plugin.Caps{CreateExclusive: true, VisibleWhenComplete: true}
}

func (s mountStore) Write(local, remote string, exclusive bool) error {
	dest := s.path(remote)
	if err := os.MkdirAll(filepath.Dir(dest), 0o755); err != nil {
		return err
	}
	in, err := os.Open(local)
	if err != nil {
		return err
	}
	defer in.Close()
	flags := os.O_WRONLY | os.O_CREATE | os.O_TRUNC
	if exclusive {
		flags = os.O_WRONLY | os.O_CREATE | os.O_EXCL
	}
	out, err := os.OpenFile(dest, flags, 0o644)
	if errors.Is(err, fs.ErrExist) {
		return plugin.ErrExists
	}
	if err != nil {
		return err
	}
	if _, err := io.Copy(out, in); err != nil {
		out.Close()
		return err
	}
	return out.Close()
}

func (s mountStore) Rename(from, to string, _ bool) error {
	return os.Rename(s.path(from), s.path(to))
}

func (s mountStore) Exists(remote string) (bool, error) {
	_, err := os.Stat(s.path(remote))
	if errors.Is(err, fs.ErrNotExist) {
		return false, nil
	}
	return err == nil, err
}

func (s mountStore) Delete(remote string) error {
	if err := os.Remove(s.path(remote)); err != nil && !errors.Is(err, fs.ErrNotExist) {
		return err
	}
	return nil
}

// httpError is a Databricks REST failure: its status and error code, and the message to show.
type httpError struct {
	status int
	code   string
	msg    string
}

func (e *httpError) Error() string { return e.msg }

// statusOf is a request's status without a body (HEAD, GET), for existence checks.
func statusOf(client *http.Client, method, url string, a *auth) (int, error) {
	bearer, err := a.bearer()
	if err != nil {
		return 0, err
	}
	req, err := http.NewRequest(method, url, nil)
	if err != nil {
		return 0, err
	}
	req.Header.Set("Authorization", "Bearer "+bearer)
	req.Header.Set("User-Agent", "dre")
	resp, err := client.Do(req)
	if err != nil {
		return 0, fmt.Errorf("can't reach Databricks: %v", err)
	}
	io.Copy(io.Discard, resp.Body)
	resp.Body.Close()
	return resp.StatusCode, nil
}

// deliver sends local to remote: /Volumes/... to a Volume (volumes.go), /Workspace/... (or
// /Users/..., /Shared/..., /Repos/...) to a workspace file (workspace.go). opts is the
// destination entry's `if_exists`.
func deliver(local, remote string, conn, opts map[string]any) (string, error) {
	if remote == "" {
		return "", fmt.Errorf("the databricks destination needs `output.destination.path`: /Volumes/<catalog>/<schema>/<volume>/<file> or /Workspace/...")
	}
	switch strings.SplitN(strings.TrimLeft(remote, "/"), "/", 2)[0] {
	case "Volumes":
		return deliverToVolume(local, remote, conn, opts)
	case "Workspace", "Users", "Shared", "Repos":
		return deliverToWorkspace(local, remote, conn, opts)
	}
	return "", fmt.Errorf("`%s` must be a Volume path (/Volumes/<catalog>/<schema>/<volume>/<file>) or a workspace file path (/Workspace/Users/<user>/..., /Workspace/Shared/... or /Workspace/Repos/...)", remote)
}
