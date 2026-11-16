package main

// uploadClient's timeout is an inactivity limit on every read and write, not on the whole transfer.

import (
	"context"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/get-dre/dre/go/plugin"
)

func idleRules(d time.Duration) plugin.Rules {
	r := plugin.DefaultRules()
	r.Timeout = d
	return r
}

func TestAReplyBodyThatStallsAfterTheHeadersFailsWithinTheTimeout(t *testing.T) {
	release := make(chan struct{})
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		io.Copy(io.Discard, r.Body)
		w.Header().Set("Content-Length", "100")
		w.WriteHeader(500)
		w.Write([]byte("partial"))
		w.(http.Flusher).Flush()
		<-release
	}))
	t.Cleanup(srv.Close)
	t.Cleanup(func() { close(release) })
	client := uploadClient(context.Background(), idleRules(300*time.Millisecond))
	start := time.Now()
	err := volumeRequest(client, srv.URL+"/api/2.0/fs/files/x", &auth{token: "good"}, localFile(t, "x"))
	if err == nil {
		t.Fatal("a stalled reply passed")
	}
	if d := time.Since(start); d > 3*time.Second {
		t.Fatalf("took %v", d)
	}
}

func TestAStalledUploadIsBoundedAndCancelable(t *testing.T) {
	release := make(chan struct{})
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		<-release // never reads the body
	}))
	t.Cleanup(srv.Close)
	t.Cleanup(func() { close(release) })
	big := filepath.Join(t.TempDir(), "big")
	if err := os.WriteFile(big, make([]byte, 64<<20), 0o600); err != nil {
		t.Fatal(err)
	}
	a := &auth{token: "good"}

	start := time.Now()
	err := volumeRequest(uploadClient(context.Background(), idleRules(300*time.Millisecond)), srv.URL+"/x", a, big)
	if err == nil || time.Since(start) > 3*time.Second {
		t.Fatalf("idle upload: %v after %v", err, time.Since(start))
	}

	ctx, cancel := context.WithCancel(context.Background())
	time.AfterFunc(200*time.Millisecond, cancel)
	start = time.Now()
	err = volumeRequest(uploadClient(ctx, idleRules(time.Hour)), srv.URL+"/x", a, big)
	if err == nil || time.Since(start) > 3*time.Second {
		t.Fatalf("cancel: %v after %v", err, time.Since(start))
	}
}

func TestASlowButProgressingReplyLongerThanTheTimeoutSucceeds(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		io.Copy(io.Discard, r.Body)
		w.Header().Set("Content-Length", "8")
		w.WriteHeader(200)
		for i := 0; i < 8; i++ {
			w.Write([]byte("x"))
			w.(http.Flusher).Flush()
			time.Sleep(100 * time.Millisecond)
		}
	}))
	t.Cleanup(srv.Close)
	client := uploadClient(context.Background(), idleRules(400*time.Millisecond))
	if err := volumeRequest(client, srv.URL+"/x", &auth{token: "good"}, localFile(t, "x")); err != nil {
		t.Fatal(err)
	}
}

func TestASlowUploadLongerThanTheTimeoutSucceeds(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		buf := make([]byte, 1<<20)
		for {
			if _, err := r.Body.Read(buf); err != nil {
				break
			}
			time.Sleep(60 * time.Millisecond)
		}
		w.WriteHeader(204)
	}))
	t.Cleanup(srv.Close)
	big := filepath.Join(t.TempDir(), "big")
	os.WriteFile(big, make([]byte, 24<<20), 0o600)
	client := uploadClient(context.Background(), idleRules(500*time.Millisecond))
	start := time.Now()
	if err := volumeRequest(client, srv.URL+"/x", &auth{token: "good"}, big); err != nil {
		t.Fatal(err)
	}
	t.Logf("took %v", time.Since(start))
}
