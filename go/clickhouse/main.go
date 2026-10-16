// Command dre-plugin-clickhouse is DRE's ClickHouse plugin package: the `clickhouse` source, on
// ClickHouse's official Go driver (clickhouse-go v2). It speaks DRE's plugin protocol
// (docs/protocol.md) on stdin and stdout through the shared Go module, and logs to stderr.
//
// Profile fields follow dbt-clickhouse's names and values, so a dbt profile can be copied across:
// `driver` (http or native), `host`, `port`, `user`, `password`, `schema` (the ClickHouse
// database; dbt's `database` also works), `secure`, `verify`, `client_cert`, `client_cert_key`,
// `connect_timeout`, `send_receive_timeout`, `compression`, `custom_settings` and `retries`, plus
// DRE's `ca_cert` and `ssh`. Fields that only matter for building models are accepted and ignored.
//
// Every statement of a Binding runs in one ClickHouse session, so temp tables and SET from one
// statement are there for the next: over HTTP a session_id, over native one held connection.
package main

import (
	"github.com/get-dre/dre/go/plugin"
)

// version is set at build time with -ldflags "-X main.version=<version>".
var version = "unreleased"

var (
	sourceRole = plugin.Role{
		Kind: "source", Name: "clickhouse", Capabilities: []string{"sessions", "check", "load", "read_only", "validate"},
		Fields: fields, IdentifierQuote: "`", Accepts: append(append([]string{}, aliases...), dbtOnly...),
		ValidateConnection: validateConnection,
		Open:               func(conn map[string]any) (plugin.Session, error) { return open(conn, readWrite) },
		OpenReadOnly:       func(conn map[string]any) (plugin.Session, error) { return open(conn, readOnly) },
		OpenSandboxed:      func(conn map[string]any) (plugin.Session, error) { return open(conn, sandboxed) },
	}
	pkg = plugin.Package{Version: version, Roles: []plugin.Role{sourceRole}}
)

func main() { plugin.Main(pkg) }

var fields = []plugin.Field{
	{Name: "driver", Description: "the protocol: http (the default) or native, ClickHouse's own TCP protocol", Default: "http", Choices: []string{"http", "native"}},
	{Name: "host", Description: "server host name", Default: "localhost"},
	{Name: "port", Description: "server port (default 8123, 8443 with secure, 9000 native, 9440 native with secure)", Kind: "integer"},
	{Name: "user", Description: "user name", Default: "default"},
	{Name: "password", Description: "password", Secret: true},
	{Name: "schema", Description: "the ClickHouse database to use (dbt's name; `database` also works)", Default: "default"},
	{Name: "secure", Description: "connect with TLS (ClickHouse Cloud needs it)", Default: false, Kind: "boolean"},
	{Name: "verify", Description: "check the server's TLS certificate", Default: true, Manual: true, Kind: "boolean"},
	{Name: "ca_cert", Description: "CA certificate to trust for the server's TLS certificate (PEM), when it isn't from a public CA", Manual: true, Kind: "path"},
	{Name: "client_cert", Description: "client certificate (PEM), for mutual TLS", Manual: true, Kind: "path"},
	{Name: "client_cert_key", Description: "the client certificate's private key (PEM)", Manual: true, Kind: "path"},
	{Name: "server_host_name", Description: "the host name to expect on the server's TLS certificate, when it isn't `host`", Manual: true},
	{Name: "connect_timeout", Description: "how long to wait for a connection (`30s`, `2m`, or seconds)", Default: "10s", Manual: true, Kind: "duration"},
	{Name: "send_receive_timeout", Description: "how long to wait on a read or write to the server (`5m`, or seconds)", Default: "300s", Manual: true, Kind: "duration"},
	{Name: "compression", Description: "how results are compressed on the wire: lz4 (the default), zstd, none, or for http gzip, deflate or br", Manual: true, Choices: compressions},
	{Name: "custom_settings", Description: "ClickHouse settings for every query, e.g. max_execution_time: 300", Manual: true, Kind: "map"},
	{Name: "retries", Description: "how many times to try connecting again after a temporary error (0: never)", Default: 3, Manual: true, Kind: "integer"},
	{Name: "ssh", Description: "reach the server through an SSH bastion (a block of settings)", Secret: true, Manual: true, Kind: "map"},
}

var compressions = []string{"lz4", "zstd", "none", "gzip", "deflate", "br"}

// aliases are dbt's other names for fields, and dbtOnly the dbt-clickhouse fields that only
// matter for building models or that Go handles itself: both accepted so a copied dbt profile
// works.
var (
	aliases = []string{"database"}
	dbtOnly = []string{
		"threads", "cluster", "database_engine", "cluster_mode", "sync_request_timeout", "compress_block_size",
		"check_exchange", "use_lw_deletes", "local_suffix", "local_db_prefix", "allow_automatic_deduplication",
		"tcp_keepalive", "reuse_connections",
	}
)
