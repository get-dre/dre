package main

// Sign-in, by dbt-bigquery's `method`. Everything goes through Google's own auth libraries;
// DRE stores nothing.

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"strings"
	"time"

	"golang.org/x/oauth2"
	"golang.org/x/oauth2/google"
	"golang.org/x/oauth2/google/externalaccount"
	"google.golang.org/api/impersonate"
	"google.golang.org/api/option"

	"github.com/get-dre/dre/go/plugin"
)

// defaultScopes are dbt-bigquery's: Drive lets queries read Sheets-backed tables.
var defaultScopes = []string{
	"https://www.googleapis.com/auth/bigquery",
	"https://www.googleapis.com/auth/cloud-platform",
	"https://www.googleapis.com/auth/drive",
}

// tokenSource signs in as the profile says.
func tokenSource(ctx context.Context, conn map[string]any) (oauth2.TokenSource, error) {
	scopes := plugin.Strings(conn, "scopes")
	if len(scopes) == 0 {
		scopes = defaultScopes
	}
	method := plugin.Optional(conn, "method")
	if method == "" {
		method = "oauth"
	}
	var ts oauth2.TokenSource
	switch method {
	case "oauth":
		creds, err := google.FindDefaultCredentials(ctx, scopes...)
		if err != nil {
			return nil, fmt.Errorf("method oauth uses gcloud's application-default credentials, and there are none: run `gcloud auth application-default login` (%v)", err)
		}
		ts = creds.TokenSource
	case "oauth-secrets":
		if tok := plugin.Optional(conn, "token"); tok != "" {
			ts = oauth2.StaticTokenSource(&oauth2.Token{AccessToken: tok, TokenType: "Bearer"})
			break
		}
		refresh, id, secret := plugin.Optional(conn, "refresh_token"), plugin.Optional(conn, "client_id"), plugin.Optional(conn, "client_secret")
		if refresh == "" || id == "" || secret == "" {
			return nil, fmt.Errorf("method oauth-secrets needs `token`, or `refresh_token` with `client_id` and `client_secret`")
		}
		uri := plugin.Optional(conn, "token_uri")
		if uri == "" {
			uri = google.Endpoint.TokenURL
		}
		cfg := &oauth2.Config{ClientID: id, ClientSecret: secret, Endpoint: oauth2.Endpoint{TokenURL: uri}, Scopes: scopes}
		ts = cfg.TokenSource(ctx, &oauth2.Token{RefreshToken: refresh})
	case "service-account":
		path, err := plugin.Required(conn, "keyfile")
		if err != nil {
			return nil, fmt.Errorf("method service-account needs `keyfile`, the path to a key file")
		}
		b, err := os.ReadFile(expandHome(path))
		if err != nil {
			return nil, fmt.Errorf("can't read the key file `%s`: %v", path, err)
		}
		creds, err := google.CredentialsFromJSONWithType(ctx, b, google.ServiceAccount, scopes...)
		if err != nil {
			return nil, fmt.Errorf("the key file `%s` isn't a service account key: %v", path, err)
		}
		ts = creds.TokenSource
	case "service-account-json":
		b, err := keyJSON(conn["keyfile_json"])
		if err != nil {
			return nil, err
		}
		creds, err := google.CredentialsFromJSONWithType(ctx, b, google.ServiceAccount, scopes...)
		if err != nil {
			return nil, fmt.Errorf("`keyfile_json` isn't a service account key: %v", err)
		}
		ts = creds.TokenSource
	case "external-oauth-wif":
		var err error
		if ts, err = workloadIdentity(ctx, conn, scopes); err != nil {
			return nil, err
		}
	default:
		return nil, fmt.Errorf("unknown `method` `%s`; use oauth, oauth-secrets, service-account, service-account-json or external-oauth-wif", method)
	}
	if sa := plugin.Optional(conn, "impersonate_service_account"); sa != "" {
		its, err := impersonate.CredentialsTokenSource(ctx, impersonate.CredentialsConfig{TargetPrincipal: sa, Scopes: scopes}, option.WithTokenSource(ts))
		if err != nil {
			return nil, fmt.Errorf("can't impersonate `%s`: %v", sa, err)
		}
		ts = its
	}
	return oauth2.ReuseTokenSource(nil, ts), nil
}

// keyJSON is keyfile_json as bytes: YAML gives a map, env_var() a string.
func keyJSON(v any) ([]byte, error) {
	switch k := v.(type) {
	case string:
		if strings.TrimSpace(k) != "" {
			return []byte(k), nil
		}
	case map[string]any:
		// dbt users often paste the key with "\n" escapes in private_key; JSON keeps them.
		return json.Marshal(k)
	}
	return nil, fmt.Errorf("method service-account-json needs `keyfile_json`, the key's JSON")
}

// workloadIdentity exchanges a token from another identity provider (Microsoft Entra) for a
// Google one, as dbt-bigquery's external-oauth-wif does.
func workloadIdentity(ctx context.Context, conn map[string]any, scopes []string) (oauth2.TokenSource, error) {
	provider, err := plugin.Required(conn, "workload_pool_provider_path")
	if err != nil {
		return nil, fmt.Errorf("method external-oauth-wif needs `workload_pool_provider_path`")
	}
	ep, _ := conn["token_endpoint"].(map[string]any)
	if ep == nil {
		return nil, fmt.Errorf("method external-oauth-wif needs `token_endpoint` with `type`, `request_url` and `request_data`")
	}
	if t := plugin.Optional(ep, "type"); t != "entra" {
		return nil, fmt.Errorf("`token_endpoint.type` `%s` isn't supported; use entra", t)
	}
	reqURL, err := plugin.Required(ep, "request_url")
	if err != nil {
		return nil, fmt.Errorf("`token_endpoint` needs `request_url`")
	}
	audience := provider
	if !strings.HasPrefix(audience, "//") {
		audience = "//iam.googleapis.com/" + strings.TrimPrefix(audience, "/")
	}
	return externalaccount.NewTokenSource(ctx, externalaccount.Config{
		Audience:                       audience,
		SubjectTokenType:               "urn:ietf:params:oauth:token-type:jwt",
		TokenURL:                       "https://sts.googleapis.com/v1/token",
		ServiceAccountImpersonationURL: plugin.Optional(conn, "service_account_impersonation_url"),
		Scopes:                         scopes,
		SubjectTokenSupplier:           entraToken{url: reqURL, data: plugin.Optional(ep, "request_data")},
	})
}

// entraToken posts request_data to request_url and returns the access token it answers with.
type entraToken struct{ url, data string }

func (e entraToken) SubjectToken(ctx context.Context, _ externalaccount.SupplierOptions) (string, error) {
	ctx, cancel := context.WithTimeout(ctx, 60*time.Second)
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, e.url, strings.NewReader(e.data))
	if err != nil {
		return "", err
	}
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return "", fmt.Errorf("can't get a token from %s: %v", hostOf(e.url), err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	var r struct {
		AccessToken string `json:"access_token"`
		Error       string `json:"error_description"`
	}
	_ = json.Unmarshal(body, &r)
	if resp.StatusCode != http.StatusOK || r.AccessToken == "" {
		msg := r.Error
		if msg == "" {
			msg = resp.Status
		}
		return "", fmt.Errorf("%s refused the token request: %s", hostOf(e.url), msg)
	}
	return r.AccessToken, nil
}

func hostOf(u string) string {
	if p, err := url.Parse(u); err == nil && p.Host != "" {
		return p.Host
	}
	return u
}

func expandHome(p string) string {
	if p == "~" || strings.HasPrefix(p, "~/") {
		if h, err := os.UserHomeDir(); err == nil {
			return h + p[1:]
		}
	}
	return p
}
