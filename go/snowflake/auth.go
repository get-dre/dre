package main

// The driver's Config from dbt-snowflake's profile fields. Every sign-in goes through the
// driver, which keeps the SSO and MFA token cache as it does for dbt.

import (
	"context"
	"crypto/rsa"
	"crypto/x509"
	"encoding/base64"
	"encoding/json"
	"encoding/pem"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"strings"
	"time"

	sf "github.com/snowflakedb/gosnowflake/v2"
	"github.com/youmark/pkcs8"

	"github.com/get-dre/dre/go/plugin"
)

func config(conn map[string]any) (*sf.Config, error) {
	account, err := plugin.Required(conn, "account")
	if err != nil {
		return nil, err
	}
	user, err := plugin.Required(conn, "user")
	if err != nil {
		return nil, err
	}
	cfg := &sf.Config{
		Account:     account,
		User:        user,
		Password:    plugin.Optional(conn, "password"),
		Role:        plugin.Optional(conn, "role"),
		Warehouse:   plugin.Optional(conn, "warehouse"),
		Database:    plugin.Optional(conn, "database"),
		Schema:      plugin.Optional(conn, "schema"),
		Host:        plugin.Optional(conn, "host"),
		Protocol:    plugin.Optional(conn, "protocol"),
		Application: "dre",
		Params:      map[string]*string{},
	}
	if p, err := plugin.Int(conn, "port", 0); err != nil {
		return nil, err
	} else {
		cfg.Port = int(p)
	}
	if tag := plugin.Optional(conn, "query_tag"); tag != "" {
		cfg.Params["query_tag"] = &tag
	}
	rules, _, err := plugin.RulesFrom(plugin.DefaultRules(), conn, nil, nil)
	if err != nil {
		return nil, err
	}
	// `retries` (the shared key); dbt's `connect_retries` also works.
	retries := int64(rules.Retries)
	if _, ok := conn["connect_retries"]; ok {
		if retries, err = plugin.Int(conn, "connect_retries", 1); err != nil {
			return nil, err
		}
	}
	cfg.LoginTimeout = rules.ConnectTimeout * time.Duration(retries+1)
	cfg.MaxRetryCount = int(retries)
	if keep, err := plugin.Bool(conn, "client_session_keep_alive", false); err != nil {
		return nil, err
	} else {
		cfg.ServerSessionKeepAlive = keep
	}
	for key, dst := range map[string]*sf.ConfigBool{
		"client_request_mfa_token":          &cfg.ClientRequestMfaToken,
		"client_store_temporary_credential": &cfg.ClientStoreTemporaryCredential,
	} {
		if _, set := conn[key]; !set {
			continue
		}
		v, err := plugin.Bool(conn, key, false)
		if err != nil {
			return nil, err
		}
		*dst = sf.ConfigBoolFalse
		if v {
			*dst = sf.ConfigBoolTrue
		}
	}
	if insecure, err := plugin.Bool(conn, "insecure_mode", false); err != nil {
		return nil, err
	} else if insecure {
		cfg.DisableOCSPChecks = true
	}
	if h := plugin.Optional(conn, "proxy_host"); h != "" {
		cfg.ProxyHost = h
		p, err := plugin.Int(conn, "proxy_port", 0)
		if err != nil {
			return nil, err
		}
		cfg.ProxyPort = int(p)
	}
	if err := signIn(cfg, conn); err != nil {
		return nil, err
	}
	return cfg, nil
}

// signIn sets the authenticator and its credentials, by dbt-snowflake's `authenticator`.
func signIn(cfg *sf.Config, conn map[string]any) error {
	auth := strings.ToLower(strings.TrimSpace(plugin.Optional(conn, "authenticator")))
	key, err := privateKey(conn)
	if err != nil {
		return err
	}
	token := plugin.Optional(conn, "token")
	switch {
	case key != nil && (auth == "" || auth == "snowflake" || auth == "snowflake_jwt" || auth == "jwt"):
		// Key-pair sign-in: the driver signs a JWT with the key.
		cfg.Authenticator = sf.AuthTypeJwt
		cfg.PrivateKey = key
		return nil
	case auth == "" || auth == "snowflake":
		if cfg.Password == "" {
			return fmt.Errorf("password sign-in needs `password` (or set `authenticator`, or a key with `private_key_path`)")
		}
		cfg.Authenticator = sf.AuthTypeSnowflake
	case auth == "username_password_mfa":
		if cfg.Password == "" {
			return fmt.Errorf("authenticator username_password_mfa needs `password`")
		}
		cfg.Authenticator = sf.AuthTypeUsernamePasswordMFA
	case auth == "externalbrowser":
		cfg.Authenticator = sf.AuthTypeExternalBrowser
	case auth == "oauth":
		if token == "" {
			return fmt.Errorf("authenticator oauth needs `token`")
		}
		if id := plugin.Optional(conn, "oauth_client_id"); id != "" {
			// As dbt does: with a client, `token` is a refresh token, exchanged for an access
			// token at the account's OAuth endpoint.
			access, err := refreshOAuth(cfg, id, plugin.Optional(conn, "oauth_client_secret"), token)
			if err != nil {
				return err
			}
			token = access
		}
		cfg.Authenticator = sf.AuthTypeOAuth
		cfg.Token = token
	case auth == "jwt":
		// A JWT from an external identity provider, sent as an OAuth token.
		if token == "" {
			return fmt.Errorf("authenticator jwt needs `token` (or a key-pair private key)")
		}
		cfg.Authenticator = sf.AuthTypeOAuth
		cfg.Token = token
	case auth == "programmatic_access_token" || auth == "pat":
		if token == "" {
			token = cfg.Password
		}
		if token == "" {
			return fmt.Errorf("authenticator programmatic_access_token needs `token`")
		}
		cfg.Authenticator = sf.AuthTypePat
		cfg.Token = token
		cfg.Password = ""
	case auth == "workload_identity":
		provider := strings.ToUpper(plugin.Optional(conn, "workload_identity_provider"))
		switch provider {
		case "AWS", "AZURE", "GCP":
		case "OIDC":
			if token == "" {
				return fmt.Errorf("workload identity with provider OIDC needs `token`")
			}
			cfg.Token = token
		default:
			return fmt.Errorf("authenticator workload_identity needs `workload_identity_provider`: AWS, AZURE, GCP or OIDC")
		}
		cfg.Authenticator = sf.AuthTypeWorkloadIdentityFederation
		cfg.WorkloadIdentityProvider = provider
		cfg.WorkloadIdentityEntraResource = plugin.Optional(conn, "workload_identity_entra_resource")
	case strings.HasPrefix(auth, "https://"):
		// Okta native: the authenticator is the Okta URL.
		u, err := url.Parse(auth)
		if err != nil || !strings.Contains(u.Host, "okta") {
			return fmt.Errorf("`authenticator` `%s` isn't an Okta URL", auth)
		}
		if cfg.Password == "" {
			return fmt.Errorf("Okta sign-in needs `password`")
		}
		cfg.Authenticator = sf.AuthTypeOkta
		cfg.OktaURL = u
	default:
		return fmt.Errorf("unknown `authenticator` `%s`; use snowflake, username_password_mfa, externalbrowser, oauth, jwt, programmatic_access_token, workload_identity or an Okta URL", auth)
	}
	return nil
}

// privateKey reads private_key or private_key_path, decrypting with private_key_passphrase.
func privateKey(conn map[string]any) (*rsa.PrivateKey, error) {
	inline, path := plugin.Optional(conn, "private_key"), plugin.Optional(conn, "private_key_path")
	if inline != "" && path != "" {
		return nil, fmt.Errorf("set `private_key` or `private_key_path`, not both")
	}
	var data []byte
	what := "`private_key`"
	switch {
	case path != "":
		b, err := os.ReadFile(expandHome(path))
		if err != nil {
			return nil, fmt.Errorf("can't read the private key `%s`: %v", path, err)
		}
		data, what = b, fmt.Sprintf("the private key `%s`", path)
	case inline != "":
		data = []byte(inline)
	default:
		return nil, nil
	}
	pass := []byte(plugin.Optional(conn, "private_key_passphrase"))
	var der []byte
	if block, _ := pem.Decode(data); block != nil {
		der = block.Bytes
	} else if b, err := base64.StdEncoding.DecodeString(strings.Join(strings.Fields(string(data)), "")); err == nil {
		// dbt also takes the key as base64 DER, without PEM armour.
		der = b
	} else {
		return nil, fmt.Errorf("%s isn't a PEM or base64 key", what)
	}
	var key any
	var err error
	if len(pass) > 0 {
		key, err = pkcs8.ParsePKCS8PrivateKey(der, pass)
	} else {
		key, err = x509.ParsePKCS8PrivateKey(der)
		if err != nil {
			key, err = x509.ParsePKCS1PrivateKey(der)
		}
	}
	if err != nil {
		if len(pass) == 0 && strings.Contains(err.Error(), "encrypted") || len(pass) == 0 && strings.Contains(string(data), "ENCRYPTED") {
			return nil, fmt.Errorf("%s is encrypted; set `private_key_passphrase`", what)
		}
		return nil, fmt.Errorf("can't read %s: %v", what, err)
	}
	rsaKey, ok := key.(*rsa.PrivateKey)
	if !ok {
		return nil, fmt.Errorf("%s isn't an RSA key; Snowflake key-pair sign-in uses RSA", what)
	}
	return rsaKey, nil
}

// refreshOAuth exchanges a refresh token for an access token at Snowflake's OAuth endpoint.
func refreshOAuth(cfg *sf.Config, id, secret, refresh string) (string, error) {
	host := cfg.Host
	if host == "" {
		host = cfg.Account + ".snowflakecomputing.com"
	}
	form := url.Values{"grant_type": {"refresh_token"}, "refresh_token": {refresh}}
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, "https://"+host+"/oauth/token-request", strings.NewReader(form.Encode()))
	if err != nil {
		return "", err
	}
	req.SetBasicAuth(id, secret)
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return "", fmt.Errorf("can't renew the OAuth token: %v", err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	var r struct {
		AccessToken string `json:"access_token"`
		Message     string `json:"message"`
		Error       string `json:"error_description"`
	}
	_ = json.Unmarshal(body, &r)
	if resp.StatusCode != http.StatusOK || r.AccessToken == "" {
		msg := r.Error
		if msg == "" {
			msg = r.Message
		}
		if msg == "" {
			msg = resp.Status
		}
		return "", fmt.Errorf("Snowflake refused to renew the OAuth token: %s", msg)
	}
	return r.AccessToken, nil
}

func expandHome(p string) string {
	if p == "~" || strings.HasPrefix(p, "~/") {
		if h, err := os.UserHomeDir(); err == nil {
			return h + p[1:]
		}
	}
	return p
}
