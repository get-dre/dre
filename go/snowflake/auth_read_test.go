package main

import (
	"errors"
	"io"
	"net/http"
	"strings"
	"testing"

	sf "github.com/snowflakedb/gosnowflake/v2"
)

type tokenRoundTripper func(*http.Request) (*http.Response, error)

func (f tokenRoundTripper) RoundTrip(r *http.Request) (*http.Response, error) { return f(r) }

type brokenTokenBody struct{ io.Reader }

func (b brokenTokenBody) Read(p []byte) (int, error) {
	n, err := b.Reader.Read(p)
	if err == io.EOF {
		return n, errors.New("token response interrupted")
	}
	return n, err
}

func TestTokenResponseRead(t *testing.T) {
	original := http.DefaultClient
	t.Cleanup(func() { http.DefaultClient = original })
	for _, c := range []struct {
		name, body    string
		status        int
		broken        bool
		want, wantErr string
	}{
		{"success", `{"access_token":"token"}`, 200, false, "token", ""},
		{"server detail", `{"error_description":"consent required"}`, 400, false, "", "consent required"},
		{"incomplete response", `{"access_token":"partial-token"}`, 200, true, "", "token response interrupted"},
	} {
		t.Run(c.name, func(t *testing.T) {
			http.DefaultClient = &http.Client{Transport: tokenRoundTripper(func(r *http.Request) (*http.Response, error) {
				var body io.Reader = strings.NewReader(c.body)
				if c.broken {
					body = brokenTokenBody{body}
				}
				return &http.Response{StatusCode: c.status, Status: http.StatusText(c.status), Body: io.NopCloser(body), Header: make(http.Header), Request: r}, nil
			})}
			got, err := refreshOAuth(&sf.Config{Host: "account.example"}, "id", "secret", "refresh")
			if got != c.want {
				t.Errorf("token = %q, want %q", got, c.want)
			}
			if c.wantErr == "" {
				if err != nil {
					t.Fatal(err)
				}
			} else if err == nil || !strings.Contains(err.Error(), c.wantErr) {
				t.Fatalf("error = %v, want %q", err, c.wantErr)
			}
		})
	}
}
