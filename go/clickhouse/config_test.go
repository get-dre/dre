package main

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/ClickHouse/clickhouse-go/v2"

	"github.com/get-dre/dre/go/plugin"
)

func TestDefaultsFollowDbtClickhouse(t *testing.T) {
	o, err := options(map[string]any{}, readWrite)
	if err != nil {
		t.Fatal(err)
	}
	if o.Protocol != clickhouse.HTTP || o.Addr[0] != "localhost:8123" || o.TLS != nil {
		t.Fatalf("%v %v %v", o.Protocol, o.Addr, o.TLS)
	}
	if o.Auth.Username != "default" || o.Auth.Database != "default" || o.Auth.Password != "" {
		t.Fatalf("%+v", o.Auth)
	}
	if o.DialTimeout != 10*time.Second || o.ReadTimeout != 300*time.Second {
		t.Fatalf("%v %v", o.DialTimeout, o.ReadTimeout)
	}
	if o.Compression == nil || o.Compression.Method != clickhouse.CompressionLZ4 {
		t.Fatalf("%v", o.Compression)
	}
	if _, ok := o.Settings["readonly"]; ok {
		t.Fatal("readonly set on a read-write session")
	}
}

func TestThePortFollowsTheDriverAndTLS(t *testing.T) {
	for _, c := range []struct {
		conn map[string]any
		addr string
	}{
		{map[string]any{"secure": true}, "localhost:8443"},
		{map[string]any{"driver": "native"}, "localhost:9000"},
		{map[string]any{"driver": "native", "secure": true}, "localhost:9440"},
		{map[string]any{"driver": "native", "secure": true, "port": 19440.0, "host": "ch.example.com"}, "ch.example.com:19440"},
	} {
		o, err := options(c.conn, readWrite)
		if err != nil {
			t.Fatal(err)
		}
		if o.Addr[0] != c.addr {
			t.Errorf("%v: %s, want %s", c.conn, o.Addr[0], c.addr)
		}
		if secure, _ := c.conn["secure"].(bool); secure != (o.TLS != nil) {
			t.Errorf("%v: TLS %v", c.conn, o.TLS)
		}
	}
}

func TestDbtsDatabaseNamesTheSchema(t *testing.T) {
	o, err := options(map[string]any{"database": "shop"}, readWrite)
	if err != nil || o.Auth.Database != "shop" {
		t.Fatalf("%v %v", o, err)
	}
	if errs := validateConnection(map[string]any{"database": "shop", "schema": "other"}); len(errs) != 1 ||
		!strings.Contains(errs[0], "`database` and `schema`") {
		t.Fatalf("%v", errs)
	}
	if errs := validateConnection(map[string]any{"database": "shop", "schema": "shop"}); len(errs) != 0 {
		t.Fatalf("%v", errs)
	}
}

func TestReadOnlySessionsSendReadonlyTwo(t *testing.T) {
	o, err := options(map[string]any{"custom_settings": map[string]any{"max_execution_time": 30.0, "readonly": 0.0}}, readOnly)
	if err != nil {
		t.Fatal(err)
	}
	// readonly=2 still allows SET and temp tables; a profile can't turn it off.
	if o.Settings["readonly"] != 2 || o.Settings["max_execution_time"] != "30" {
		t.Fatalf("%v", o.Settings)
	}
	if o, _ := options(map[string]any{}, sandboxed); o.Settings["readonly"] != 1 {
		t.Fatalf("%v", o.Settings)
	}
}

func TestTLSSettings(t *testing.T) {
	dir := t.TempDir()
	ca := filepath.Join(dir, "ca.pem")
	os.WriteFile(ca, []byte("not a certificate"), 0o600)
	o, err := options(map[string]any{"secure": true, "verify": false, "server_host_name": "ch.internal"}, readWrite)
	if err != nil {
		t.Fatal(err)
	}
	if !o.TLS.InsecureSkipVerify || o.TLS.ServerName != "ch.internal" {
		t.Fatalf("%+v", o.TLS)
	}
	if _, err := options(map[string]any{"secure": true, "ca_cert": ca}, readWrite); err == nil || !strings.Contains(err.Error(), "ca_cert") {
		t.Fatalf("%v", err)
	}
	if errs := validateConnection(map[string]any{"secure": true, "client_cert": ca}); len(errs) != 1 || !strings.Contains(errs[0], "`client_cert_key`") {
		t.Fatalf("%v", errs)
	}
	if errs := validateConnection(map[string]any{"verify": false}); len(errs) != 1 || !strings.Contains(errs[0], "`secure: true`") {
		t.Fatalf("%v", errs)
	}
}

func TestCompressionAndTimeouts(t *testing.T) {
	o, err := options(map[string]any{"compression": "none", "connect_timeout": "2s", "send_receive_timeout": 60.0}, readWrite)
	if err != nil {
		t.Fatal(err)
	}
	if o.Compression != nil || o.DialTimeout != 2*time.Second || o.ReadTimeout != time.Minute {
		t.Fatalf("%v %v %v", o.Compression, o.DialTimeout, o.ReadTimeout)
	}
	if errs := validateConnection(map[string]any{"driver": "native", "compression": "gzip"}); len(errs) != 1 ||
		!strings.Contains(errs[0], "`compression: gzip` works only with `driver: http`") {
		t.Fatalf("%v", errs)
	}
}

func TestACopiedDbtProfileValidatesWithoutWarnings(t *testing.T) {
	conn := map[string]any{
		"type": "clickhouse", "driver": "native", "host": "h", "port": 9000.0, "user": "u", "password": "p",
		"schema": "s", "database": "s", "cluster": "c", "threads": 4.0, "use_lw_deletes": true, "tcp_keepalive": true,
		"custom_settings": map[string]any{"a": 1.0}, "retries": 1.0, "connect_timeout": 10.0,
	}
	delete(conn, "type")
	errs, warns := plugin.CheckConnection(sourceRole, conn, nil)
	if len(errs)+len(warns) != 0 {
		t.Fatalf("%v %v", errs, warns)
	}
}
