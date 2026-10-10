package main

// The databricks destination's workspace paths: writes the output as a workspace file
// (/Workspace/Users/..., /Workspace/Shared/..., /Workspace/Repos/...) through the Workspace API,
// creating missing folders. On Databricks compute, where /Workspace is mounted, it copies the
// file there instead, with the job or cluster's own access. Sign-in is the same as the source's.
//
// Files are imported as plain files (format RAW), never converted to notebooks, and replace
// an existing file at the path unless `if_exists` says otherwise.

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"mime/multipart"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"strings"

	"github.com/get-dre/dre/go/plugin"
)

// workspacePath checks remote and returns the path as the Workspace API takes it (without the
// /Workspace prefix) and as shown to the person (with it).
func workspacePath(remote string) (api, shown string, err error) {
	p := "/" + strings.Trim(remote, "/")
	api = strings.TrimPrefix(p, "/Workspace")
	parts := strings.Split(strings.TrimPrefix(api, "/"), "/")
	top := map[string]bool{"Users": true, "Shared": true, "Repos": true}
	bad := len(parts) < 2 || !top[parts[0]] || !strings.HasPrefix(p, "/Workspace/") && !strings.HasPrefix(p, "/"+parts[0]+"/")
	for _, s := range parts {
		bad = bad || s == "" || s == "." || s == ".."
	}
	if bad {
		return "", "", fmt.Errorf("`%s` must be a workspace file path: /Workspace/Users/<user>/..., /Workspace/Shared/... or /Workspace/Repos/...", remote)
	}
	return api, "/Workspace" + api, nil
}

func deliverToWorkspace(local, remote string, conn, opts map[string]any) (string, error) {
	if remote == "" {
		return "", fmt.Errorf("the databricks destination needs `output.destination.path`")
	}
	api, shown, err := workspacePath(remote)
	if err != nil {
		return "", err
	}
	rules, err := rulesFor(conn, opts)
	if err != nil {
		return "", err
	}
	if root, ok := mountedWorkspace(); ok {
		d, err := plugin.Deliver(mountStore{root}, local, shown, rules)
		if err != nil {
			return "", failed("copy to", shown, err)
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
	a, err := authFromConn(conn, base)
	if err != nil {
		return "", err
	}
	client := &http.Client{}
	dir := api[:strings.LastIndex(api, "/")]
	mkdirs, _ := json.Marshal(map[string]string{"path": dir})
	_, err = plugin.Retry(rules.Retries, "creating /Workspace"+dir, func() (struct{}, error) {
		return struct{}{}, workspaceRequest(client, base+"/api/2.0/workspace/mkdirs", a, func() (io.Reader, string, error) {
			return bytes.NewReader(mkdirs), "application/json", nil
		})
	})
	if err != nil {
		return "", fmt.Errorf("can't create /Workspace%s: %v", dir, err)
	}
	d, err := plugin.Deliver(&workspaceStore{client, base, a}, local, shown, rules)
	if err != nil {
		return "", failed("upload to", shown, err)
	}
	return d.Path, nil
}

// workspaceStore is the Workspace API, as the shared delivery rules' store; paths are as shown
// (/Workspace/...). An import with `overwrite: false` refuses a file already there in the same
// step (RESOURCE_ALREADY_EXISTS).
type workspaceStore struct {
	client *http.Client
	base   string
	a      *auth
}

func (*workspaceStore) Caps() plugin.Caps {
	return plugin.Caps{CreateExclusive: true, VisibleWhenComplete: true}
}

func (s *workspaceStore) Write(local, remote string, exclusive bool) error {
	api := strings.TrimPrefix(remote, "/Workspace")
	err := workspaceRequest(s.client, s.base+"/api/2.0/workspace/import", s.a, func() (io.Reader, string, error) {
		return importForm(local, api, !exclusive)
	})
	var he *httpError
	if errors.As(err, &he) && he.code == "RESOURCE_ALREADY_EXISTS" {
		return plugin.ErrExists
	}
	return err
}

func (*workspaceStore) Rename(_, to string, _ bool) error {
	return fmt.Errorf("can't rename to %s: workspace files need no temporary name", to)
}

func (s *workspaceStore) Exists(remote string) (bool, error) {
	api := strings.TrimPrefix(remote, "/Workspace")
	st, err := statusOf(s.client, "GET", s.base+"/api/2.0/workspace/get-status?path="+percentEncode(api, false), s.a)
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

func (s *workspaceStore) Delete(remote string) error {
	b, _ := json.Marshal(map[string]string{"path": strings.TrimPrefix(remote, "/Workspace")})
	return workspaceRequest(s.client, s.base+"/api/2.0/workspace/delete", s.a, func() (io.Reader, string, error) {
		return bytes.NewReader(b), "application/json", nil
	})
}

// importForm is the multipart body of a workspace import: the file as is, replacing any file
// already there when overwrite. The file is streamed, so large outputs don't sit in memory.
func importForm(local, api string, overwrite bool) (io.Reader, string, error) {
	f, err := os.Open(local)
	if err != nil {
		return nil, "", fmt.Errorf("can't read %s: %v", local, err)
	}
	pr, pw := io.Pipe()
	mw := multipart.NewWriter(pw)
	go func() {
		defer f.Close()
		fields := [][2]string{{"path", api}, {"format", "RAW"}, {"overwrite", strconv.FormatBool(overwrite)}}
		for _, kv := range fields {
			if err := mw.WriteField(kv[0], kv[1]); err != nil {
				pw.CloseWithError(err)
				return
			}
		}
		part, err := mw.CreateFormFile("content", filepath.Base(local))
		if err == nil {
			_, err = io.Copy(part, f)
		}
		if err == nil {
			err = mw.Close()
		}
		pw.CloseWithError(err)
	}()
	return pr, mw.FormDataContentType(), nil
}

// workspaceRequest POSTs the body from mk, once. A 429, 503 or connection failure comes back as
// a *plugin.TemporaryError, for the delivery rules to retry.
func workspaceRequest(client *http.Client, url string, a *auth, mk func() (io.Reader, string, error)) error {
	bearer, err := a.bearer()
	if err != nil {
		return err
	}
	body, ctype, err := mk()
	if err != nil {
		return err
	}
	req, err := http.NewRequest("POST", url, body)
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+bearer)
	req.Header.Set("User-Agent", "dre")
	req.Header.Set("Content-Type", ctype)
	resp, err := client.Do(req)
	if c, ok := body.(io.Closer); ok {
		c.Close()
	}
	if err != nil {
		return temporary(fmt.Errorf("can't reach Databricks: %v", err), 0, "")
	}
	text, _ := io.ReadAll(resp.Body)
	resp.Body.Close()
	if resp.StatusCode >= 200 && resp.StatusCode < 300 {
		return nil
	}
	hint := ""
	switch resp.StatusCode {
	case 401, 403:
		hint = " (check the token and its permissions on the folder)"
		if a.oauth != nil {
			hint = " (check the signed-in identity's permissions on the folder)"
		}
	case 404:
		hint = " (check the user or repo folder exists)"
	}
	herr := &httpError{resp.StatusCode, apiErrorCode(text), fmt.Sprintf("HTTP %d%s: %s", resp.StatusCode, hint, apiError(text))}
	return temporary(herr, resp.StatusCode, resp.Header.Get("Retry-After"))
}

// mountedWorkspace is the root to write /Workspace/... under on Databricks compute, where the
// workspace is mounted; ok is false when it doesn't apply. DRE_WORKSPACE_ROOT stands in for / in
// tests.
func mountedWorkspace() (root string, ok bool) {
	if os.Getenv("DATABRICKS_RUNTIME_VERSION") == "" {
		return "", false
	}
	root = os.Getenv("DRE_WORKSPACE_ROOT")
	if root == "" {
		root = "/"
	}
	if st, err := os.Stat(filepath.Join(root, "Workspace")); err != nil || !st.IsDir() {
		return "", false
	}
	return root, true
}
