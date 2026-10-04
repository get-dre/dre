package main

import (
	"context"
	"crypto/rand"
	"crypto/rsa"
	"crypto/x509"
	"encoding/json"
	"encoding/pem"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"golang.org/x/oauth2/google/externalaccount"

	"github.com/get-dre/dre/go/plugin/plugintest"
)

func errOf(t *testing.T, conn map[string]any) string {
	t.Helper()
	_, err := tokenSource(context.Background(), conn)
	if err == nil {
		return ""
	}
	return err.Error()
}

// serviceAccountKey is a key file whose token endpoint is tokenURL.
func serviceAccountKey(t *testing.T, tokenURL string) map[string]any {
	k, _ := rsa.GenerateKey(rand.Reader, 2048)
	der, _ := x509.MarshalPKCS8PrivateKey(k)
	return map[string]any{
		"type": "service_account", "project_id": "p", "private_key_id": "1",
		"private_key":  string(pem.EncodeToMemory(&pem.Block{Type: "PRIVATE KEY", Bytes: der})),
		"client_email": "sa@p.iam.gserviceaccount.com", "client_id": "1", "token_uri": tokenURL,
	}
}

func TestEachMethodSignsInOrSaysWhatsMissing(t *testing.T) {
	cases := []struct {
		conn map[string]any
		want string
	}{
		{map[string]any{"method": "saml"}, "unknown `method` `saml`"},
		{map[string]any{"method": "oauth-secrets"}, "needs `token`, or `refresh_token` with `client_id` and `client_secret`"},
		{map[string]any{"method": "oauth-secrets", "refresh_token": "r"}, "needs `token`, or `refresh_token`"},
		{map[string]any{"method": "service-account"}, "needs `keyfile`"},
		{map[string]any{"method": "service-account", "keyfile": "/nope.json"}, "can't read the key file `/nope.json`"},
		{map[string]any{"method": "service-account-json"}, "needs `keyfile_json`"},
		{map[string]any{"method": "service-account-json", "keyfile_json": `{"type": "authorized_user"}`}, "isn't a service account key"},
		{map[string]any{"method": "external-oauth-wif"}, "needs `workload_pool_provider_path`"},
		{map[string]any{"method": "external-oauth-wif", "workload_pool_provider_path": "p"}, "needs `token_endpoint`"},
		{map[string]any{"method": "external-oauth-wif", "workload_pool_provider_path": "p", "token_endpoint": map[string]any{"type": "okta"}}, "`token_endpoint.type` `okta` isn't supported"},
		{map[string]any{"method": "oauth-secrets", "token": "t"}, ""},
		{map[string]any{"method": "oauth-secrets", "refresh_token": "r", "client_id": "c", "client_secret": "s"}, ""},
	}
	for _, c := range cases {
		if got := errOf(t, c.conn); (c.want == "" && got != "") || !strings.Contains(got, c.want) {
			t.Errorf("%v: got %q, want %q", c.conn, got, c.want)
		}
	}
}

func TestServiceAccountKeysFromAFileOrInline(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.Write([]byte(`{"access_token": "from-key", "token_type": "Bearer", "expires_in": 3600}`))
	}))
	defer srv.Close()
	key := serviceAccountKey(t, srv.URL)
	b, _ := json.Marshal(key)
	path := filepath.Join(t.TempDir(), "key.json")
	os.WriteFile(path, b, 0o600)
	for _, conn := range []map[string]any{
		{"method": "service-account", "keyfile": path},
		{"method": "service-account-json", "keyfile_json": key},
		{"method": "service-account-json", "keyfile_json": string(b)},
	} {
		ts, err := tokenSource(context.Background(), conn)
		if err != nil {
			t.Fatal(err)
		}
		tok, err := ts.Token()
		if err != nil || tok.AccessToken != "from-key" {
			t.Fatalf("%v %v", tok, err)
		}
	}
}

func TestEntraTokensAreExchangedForGoogleOnes(t *testing.T) {
	var form string
	entra := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		r.ParseForm()
		form = r.PostForm.Get("client_id")
		w.Write([]byte(`{"access_token": "entra-jwt"}`))
	}))
	defer entra.Close()
	tok, err := entraToken{url: entra.URL, data: "grant_type=client_credentials&client_id=abc"}.SubjectToken(context.Background(), externalaccount.SupplierOptions{})
	if err != nil || tok != "entra-jwt" || form != "abc" {
		t.Fatalf("%q %v %q", tok, err, form)
	}
}

func TestProfilesNeedAProjectAndKnownFields(t *testing.T) {
	if _, err := open(map[string]any{"dataset": "d"}); err == nil || !strings.Contains(err.Error(), "needs a `project` field") {
		t.Fatalf("%v", err)
	}
	if _, err := open(map[string]any{"project": "p", "datset": "d"}); err == nil || !strings.Contains(err.Error(), "unknown profile field(s) `datset`") {
		t.Fatalf("%v", err)
	}
	if _, err := open(map[string]any{"project": "p", "priority": "urgent"}); err == nil || !strings.Contains(err.Error(), "interactive or batch") {
		t.Fatalf("%v", err)
	}
	if _, err := open(map[string]any{"project": "p", "maximum_bytes_billed": "lots"}); err == nil || !strings.Contains(err.Error(), "whole number") {
		t.Fatalf("%v", err)
	}
	// A dbt profile's model-building fields are accepted.
	if _, err := open(map[string]any{"database": "p", "threads": 4.0, "gcs_bucket": "b", "method": "nope"}); err == nil || !strings.Contains(err.Error(), "unknown `method`") {
		t.Fatalf("%v", err)
	}
}

func TestTheSourceDescribesItself(t *testing.T) {
	c := plugintest.Start(t, pkg, sourceRole)
	h := c.Hello()
	if h["name"] != "bigquery" || h["kind"] != "source" {
		t.Fatalf("%v", h)
	}
	c.Send(map[string]any{"type": "describe"})
	d := c.Reply()
	if d["identifier_quote"] != "`" || !strings.HasPrefix(plugintest.FieldNames(d), "method,project,dataset,location,keyfile,") {
		t.Fatalf("%v", d)
	}
}
