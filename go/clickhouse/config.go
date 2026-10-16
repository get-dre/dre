package main

import (
	"crypto/tls"
	"crypto/x509"
	"fmt"
	"net"
	"os"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/ClickHouse/clickhouse-go/v2"

	"github.com/get-dre/dre/go/plugin"
)

// Read-only levels, ClickHouse's `readonly` setting: a report's read-only session refuses
// writes and DDL on tables but keeps SET and temp tables; `dre query`'s sandbox also refuses SET,
// temp tables and the table functions that reach outside the database (url, file, s3, remote,
// ...). Once a query carries one, neither SET nor a SETTINGS clause can lower it; the profile's
// own settings, sent with it, still apply.
type readOnlyLevel int

const (
	readWrite readOnlyLevel = 0
	sandboxed readOnlyLevel = 1
	readOnly  readOnlyLevel = 2
)

// options is the driver's configuration from a profile target, at a read-only level. Sessions,
// dialing through SSH and query IDs are added by open.
func options(conn map[string]any, level readOnlyLevel) (*clickhouse.Options, error) {
	native := plugin.Optional(conn, "driver") == "native"
	secure, err := plugin.Bool(conn, "secure", false)
	if err != nil {
		return nil, err
	}
	def := map[[2]bool]int64{{false, false}: 8123, {false, true}: 8443, {true, false}: 9000, {true, true}: 9440}[[2]bool{native, secure}]
	port, err := plugin.Int(conn, "port", def)
	if err != nil {
		return nil, err
	}
	host := plugin.Optional(conn, "host")
	if host == "" {
		host = "localhost"
	}
	o := &clickhouse.Options{
		Protocol: clickhouse.HTTP,
		Addr:     []string{net.JoinHostPort(host, strconv.FormatInt(port, 10))},
		Auth: clickhouse.Auth{
			Username: plugin.Optional(conn, "user"),
			Password: plugin.Optional(conn, "password"),
			Database: database(conn),
		},
		ClientInfo: clickhouse.ClientInfo{Products: []struct{ Name, Version string }{{Name: "dre", Version: version}}},
		Settings:   clickhouse.Settings{},
		// One connection: over native it is the session.
		MaxOpenConns:    1,
		MaxIdleConns:    1,
		ConnMaxLifetime: 24 * time.Hour,
	}
	if native {
		o.Protocol = clickhouse.Native
	}
	if o.Auth.Username == "" {
		o.Auth.Username = "default"
	}
	if o.DialTimeout, err = duration(conn, "connect_timeout", 10*time.Second); err != nil {
		return nil, err
	}
	if o.ReadTimeout, err = duration(conn, "send_receive_timeout", 300*time.Second); err != nil {
		return nil, err
	}
	name := plugin.Optional(conn, "compression")
	if name == "" {
		name = "lz4"
	}
	c, ok := compressions[name]
	if !ok {
		return nil, fmt.Errorf("`compression` must be one of %s", strings.Join(compressionNames(), ", "))
	}
	if c.method != clickhouse.CompressionNone {
		o.Compression = &clickhouse.Compression{Method: c.method}
	}
	if s, ok := conn["custom_settings"].(map[string]any); ok {
		for k, v := range s {
			o.Settings[k] = settingValue(v)
		}
	}
	if level != readWrite {
		o.Settings["readonly"] = int(level)
	}
	if secure {
		if o.TLS, err = tlsConfig(conn); err != nil {
			return nil, err
		}
	}
	return o, nil
}

// compressions are the `compression` values: the driver's method, and whether only HTTP has it.
var compressions = map[string]struct {
	method   clickhouse.CompressionMethod
	httpOnly bool
}{
	"lz4":     {method: clickhouse.CompressionLZ4},
	"zstd":    {method: clickhouse.CompressionZSTD},
	"none":    {method: clickhouse.CompressionNone},
	"gzip":    {method: clickhouse.CompressionGZIP, httpOnly: true},
	"deflate": {method: clickhouse.CompressionDeflate, httpOnly: true},
	"br":      {method: clickhouse.CompressionBrotli, httpOnly: true},
}

// compressionNames are the `compression` values in the order the docs give them.
func compressionNames() []string { return []string{"lz4", "zstd", "none", "gzip", "deflate", "br"} }

// database is `schema`, or dbt's `database`, or `default`.
func database(conn map[string]any) string {
	if s := plugin.Optional(conn, "schema"); s != "" {
		return s
	}
	if d := plugin.Optional(conn, "database"); d != "" {
		return d
	}
	return "default"
}

func duration(conn map[string]any, key string, def time.Duration) (time.Duration, error) {
	v, ok := conn[key]
	if !ok || v == nil {
		return def, nil
	}
	d, err := plugin.ParseDuration(v)
	if err != nil {
		return 0, fmt.Errorf("`%s` must be a duration such as `30s` or `2m`, or seconds", key)
	}
	return d, nil
}

// settingValue is a custom setting as ClickHouse takes it: numbers without a trailing `.0`.
func settingValue(v any) any {
	switch n := v.(type) {
	case float64:
		return strconv.FormatFloat(n, 'f', -1, 64)
	case bool:
		if n {
			return "1"
		}
		return "0"
	}
	return fmt.Sprint(v)
}

func tlsConfig(conn map[string]any) (*tls.Config, error) {
	verify, err := plugin.Bool(conn, "verify", true)
	if err != nil {
		return nil, err
	}
	c := &tls.Config{InsecureSkipVerify: !verify, ServerName: plugin.Optional(conn, "server_host_name")}
	if path := plugin.Optional(conn, "ca_cert"); path != "" {
		pem, err := os.ReadFile(expandHome(path))
		if err != nil {
			return nil, fmt.Errorf("can't read `ca_cert`: %v", err)
		}
		c.RootCAs = x509.NewCertPool()
		if !c.RootCAs.AppendCertsFromPEM(pem) {
			return nil, fmt.Errorf("`ca_cert` holds no PEM certificate")
		}
	}
	if cert := plugin.Optional(conn, "client_cert"); cert != "" {
		pair, err := tls.LoadX509KeyPair(expandHome(cert), expandHome(plugin.Optional(conn, "client_cert_key")))
		if err != nil {
			return nil, fmt.Errorf("can't load `client_cert` and `client_cert_key`: %v", err)
		}
		c.Certificates = []tls.Certificate{pair}
	}
	return c, nil
}

// validateConnection checks what the field declarations can't: keys that only go together.
func validateConnection(conn map[string]any) []string {
	var errs []string
	set := func(k string) bool { v, ok := conn[k]; return ok && v != nil }
	if set("database") && set("schema") && fmt.Sprint(conn["database"]) != fmt.Sprint(conn["schema"]) {
		errs = append(errs, "`database` and `schema` both name the ClickHouse database and must agree (dbt-clickhouse's rule); set one")
	}
	if set("client_cert") != set("client_cert_key") {
		errs = append(errs, "`client_cert` and `client_cert_key` go together; set both")
	}
	if secure, _ := plugin.Bool(conn, "secure", false); !secure {
		var tlsOnly []string
		for _, k := range []string{"verify", "ca_cert", "client_cert", "client_cert_key", "server_host_name"} {
			if set(k) {
				tlsOnly = append(tlsOnly, "`"+k+"`")
			}
		}
		if len(tlsOnly) > 0 {
			sort.Strings(tlsOnly)
			verb := "applies"
			if len(tlsOnly) > 1 {
				verb = "apply"
			}
			errs = append(errs, fmt.Sprintf("%s only %s with `secure: true`", joinAnd(tlsOnly), verb))
		}
	}
	if c := plugin.Optional(conn, "compression"); plugin.Optional(conn, "driver") == "native" && compressions[c].httpOnly {
		errs = append(errs, fmt.Sprintf("`compression: %s` works only with `driver: http`; native takes lz4, zstd or none", c))
	}
	if s, ok := conn["ssh"].(map[string]any); ok {
		errs = append(errs, checkSSH(s)...)
	}
	return errs
}

func joinAnd(items []string) string {
	switch len(items) {
	case 1:
		return items[0]
	case 2:
		return items[0] + " and " + items[1]
	}
	out := ""
	for i, s := range items {
		switch {
		case i == len(items)-1:
			out += ", and " + s
		case i > 0:
			out += ", " + s
		default:
			out = s
		}
	}
	return out
}
