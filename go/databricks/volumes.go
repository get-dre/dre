package main

// The databricks destination's Volume paths: uploads to a Unity Catalog Volume through the
// Databricks Files API from anywhere (a laptop, Airflow, CI). On Databricks compute, where
// /Volumes/... is a mounted path, it copies the file there instead: no API call and no login, with
// whatever access the job or cluster's identity has. The same report and profile work in both
// places.
//
// The remote path must be /Volumes/<catalog>/<schema>/<volume>/...; missing directories are
// created. Retries while the workspace answers 429/503.

import (
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/get-dre/dre/go/plugin"
)

func volumesFields() []plugin.Field {
	f := connectionFields()
	out := []plugin.Field{{Name: "host", Description: "workspace host, e.g. adb-123.4.azuredatabricks.net", Required: true, SameAsSource: "databricks"}}
	for _, c := range f {
		if c.SameAsSource == "databricks" {
			out = append(out, c)
		}
	}
	return out
}

func deliverToVolume(local, remote string, conn, opts map[string]any) (string, error) {
	if remote == "" {
		return "", fmt.Errorf("the databricks destination needs `output.destination.path`")
	}
	parts := strings.Split(strings.TrimLeft(remote, "/"), "/")
	bad := len(parts) < 5 || parts[0] != "Volumes"
	for _, p := range parts {
		bad = bad || p == ""
	}
	if bad {
		return "", fmt.Errorf("`%s` must be /Volumes/<catalog>/<schema>/<volume>/<file>", remote)
	}
	path := "/" + strings.Join(parts, "/")
	rules, err := rulesFor(opts)
	if err != nil {
		return "", err
	}
	if root, ok := mountedVolume(parts); ok {
		d, err := plugin.Deliver(mountStore{root}, local, path, rules)
		if err != nil {
			return "", failed("copy to", path, err)
		}
		return d.Path, nil
	}
	host, err := required(conn, "host")
	if err != nil {
		return "", err
	}
	base := baseURL(host)
	if err := checkAuthType(conn); err != nil {
		return "", err
	}
	if err := reachable(base); err != nil {
		return "", err
	}
	// Signs in only now, when a report actually delivers here.
	a, err := authFromConn(conn, base)
	if err != nil {
		return "", err
	}
	client := &http.Client{}
	// Directories under the volume (the volume itself must exist).
	if len(parts) > 5 {
		dir := "/" + strings.Join(parts[:len(parts)-1], "/")
		if err := volumeRequest(client, base+"/api/2.0/fs/directories"+percentEncode(dir, true), a, ""); err != nil {
			return "", fmt.Errorf("can't create %s: %v", dir, err)
		}
	}
	d, err := plugin.Deliver(&volumeStore{client, base, a}, local, path, rules)
	if err != nil {
		return "", failed("upload to", path, err)
	}
	return "dbfs:" + d.Path, nil
}

// volumeStore is the Files API, as the shared delivery rules' store: `overwrite=false` refuses
// a file already there in the same step (409).
type volumeStore struct {
	client *http.Client
	base   string
	a      *auth
}

func (*volumeStore) Caps() plugin.Caps {
	return plugin.Caps{CreateExclusive: true, VisibleWhenComplete: true}
}

func (s *volumeStore) Write(local, remote string, exclusive bool) error {
	url := fmt.Sprintf("%s/api/2.0/fs/files%s?overwrite=%t", s.base, percentEncode(remote, true), !exclusive)
	err := volumeRequest(s.client, url, s.a, local)
	var he *httpError
	if errors.As(err, &he) && he.status == 409 {
		return plugin.ErrExists
	}
	return err
}

func (*volumeStore) Rename(_, to string, _ bool) error {
	return fmt.Errorf("can't rename to %s: Volumes need no temporary name", to)
}

func (s *volumeStore) Exists(remote string) (bool, error) {
	st, err := statusOf(s.client, "HEAD", s.base+"/api/2.0/fs/files"+percentEncode(remote, true), s.a)
	switch {
	case err != nil:
		return false, err
	case st == 404:
		return false, nil
	case st >= 200 && st < 300:
		return true, nil
	}
	return false, fmt.Errorf("can't look for %s: HTTP %d", remote, st)
}

func (s *volumeStore) Delete(remote string) error {
	_, err := statusOf(s.client, "DELETE", s.base+"/api/2.0/fs/files"+percentEncode(remote, true), s.a)
	return err
}

// mountedVolume is the root to write /Volumes/... under when this runs on Databricks compute
// and the volume is mounted; ok is false when it doesn't apply, so the Files API is used
// instead. DRE_VOLUMES_ROOT stands in for / in tests.
func mountedVolume(parts []string) (root string, ok bool) {
	if os.Getenv("DATABRICKS_RUNTIME_VERSION") == "" {
		return "", false
	}
	root = os.Getenv("DRE_VOLUMES_ROOT")
	if root == "" {
		root = "/"
	}
	volume := filepath.Join(append([]string{root}, parts[:4]...)...)
	if st, err := os.Stat(volume); err != nil || !st.IsDir() {
		return "", false
	}
	return root, true
}

// volumeRequest PUTs to url (with the file at local as the body, when set), retrying while the
// workspace answers 429/503.
func volumeRequest(client *http.Client, url string, a *auth, local string) error {
	start := time.Now()
	wait := time.Second
	for {
		bearer, err := a.bearer()
		if err != nil {
			return err
		}
		var body io.Reader = http.NoBody
		var size int64
		var f *os.File
		if local != "" {
			f, err = os.Open(local)
			if err != nil {
				return fmt.Errorf("can't read %s: %v", local, err)
			}
			st, err := f.Stat()
			if err != nil {
				f.Close()
				return err
			}
			body, size = f, st.Size()
		}
		req, err := http.NewRequest("PUT", url, body)
		if err != nil {
			if f != nil {
				f.Close()
			}
			return err
		}
		req.Header.Set("Authorization", "Bearer "+bearer)
		req.Header.Set("User-Agent", "dre")
		if f != nil {
			req.ContentLength = size
			req.Header.Set("Content-Type", "application/octet-stream")
		}
		resp, err := client.Do(req)
		if f != nil {
			f.Close()
		}
		if err != nil {
			return fmt.Errorf("can't reach Databricks: %v", err)
		}
		text, _ := io.ReadAll(resp.Body)
		resp.Body.Close()
		if resp.StatusCode >= 200 && resp.StatusCode < 300 {
			return nil
		}
		if (resp.StatusCode == 429 || resp.StatusCode == 503) && time.Since(start) < 300*time.Second {
			time.Sleep(wait)
			wait = min(wait*2, 30*time.Second)
			continue
		}
		hint := ""
		switch resp.StatusCode {
		case 401, 403:
			hint = " (check the token and its permissions on the volume)"
			if a.oauth != nil {
				hint = " (check the signed-in identity's permissions on the volume)"
			}
		case 404:
			hint = " (check the catalog, schema and volume exist)"
		}
		return &httpError{resp.StatusCode, apiErrorCode(text), fmt.Sprintf("HTTP %d%s: %s", resp.StatusCode, hint, apiError(text))}
	}
}

// apiError is the readable part of a Databricks REST error: `ERROR_CODE: message` from its JSON
// body, or the start of the body when it isn't one.
func apiError(body []byte) string {
	var e struct {
		Code    string `json:"error_code"`
		Message string `json:"message"`
	}
	if json.Unmarshal(body, &e) == nil && e.Message != "" {
		if e.Code != "" {
			return e.Code + ": " + e.Message
		}
		return e.Message
	}
	return truncate(strings.TrimSpace(string(body)), 300)
}

// apiErrorCode is a Databricks REST error's `error_code`, or "".
func apiErrorCode(body []byte) string {
	var e struct {
		Code string `json:"error_code"`
	}
	json.Unmarshal(body, &e)
	return e.Code
}

// percentEncode escapes everything but RFC 3986 unreserved characters (and / when keepSlash).
func percentEncode(s string, keepSlash bool) string {
	var b strings.Builder
	for i := 0; i < len(s); i++ {
		c := s[i]
		switch {
		case c >= 'A' && c <= 'Z', c >= 'a' && c <= 'z', c >= '0' && c <= '9', c == '-', c == '_', c == '.', c == '~':
			b.WriteByte(c)
		case c == '/' && keepSlash:
			b.WriteByte(c)
		default:
			fmt.Fprintf(&b, "%%%02X", c)
		}
	}
	return b.String()
}
