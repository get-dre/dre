// Command dre-plugin-databricks is DRE's Databricks plugin package: one program serving
//
//   - the `databricks` source, for Databricks SQL warehouses, and
//   - the `databricks` destination, for Unity Catalog Volumes (/Volumes/...) and workspace files
//     (/Workspace/...), chosen by the path.
//
// Core's hello names the plugin it wants; without one, a program named dre-destination-* serves
// the destination and any other the source. Both share one sign-in (PAT or OAuth) and one OAuth
// session per workspace in ~/.dre/oauth_sessions.json.
//
// It is written in Go so the source can use Databricks' official Go connector
// (databricks-sql-go): SQL warehouses only hold sessions for Databricks' own clients, and one
// session per Binding is what keeps temp views and SETs alive between a report's queries. It
// speaks DRE's plugin protocol (docs/protocol.md) on stdin/stdout and logs to stderr.
//
// Source profile fields: host, http_path, auth_type (pat, the default, or oauth), token for pat,
// client_id / client_secret / scopes / redirect_port for oauth, optional catalog, schema and
// retry_timeout (seconds to keep retrying while a stopped warehouse starts; default 900).
// Destination profile fields: host and the same sign-in fields.
package main

import (
	"os"

	sdklog "github.com/databricks/databricks-sdk-go/logger"
	dbsqllog "github.com/databricks/databricks-sql-go/logger"

	"github.com/get-dre/dre/go/plugin"
)

// version is set at build time with -ldflags "-X main.version=<version>".
var version = "unreleased"

var (
	sourceRole = plugin.Role{
		Kind: "source", Name: "databricks", Capabilities: []string{"sessions", "check", "load", "validate"},
		Fields: connectionFields(), IdentifierQuote: "`",
		Open: func(conn map[string]any) (plugin.Session, error) { return newDatabricks(conn) },
	}
	destinationRole = plugin.Role{
		Kind: "destination", Name: "databricks", Capabilities: []string{"validate"},
		Fields: volumesFields(), Deliver: deliver,
	}
	// pkg is every plugin the package provides, in the order `provides` lists them.
	pkg = plugin.Package{Version: version, Roles: []plugin.Role{sourceRole, destinationRole}}
)

func main() {
	// The connector and SDK log their own copy of every error, and warnings DRE has no use for;
	// DRE reports errors itself. Set DATABRICKS_LOG_LEVEL (e.g. debug) to see the connector's log.
	quietLibraries()
	plugin.Main(pkg)
}

// quietLibraries turns off the connector's and SDK's own logs unless DATABRICKS_LOG_LEVEL is set.
func quietLibraries() {
	if os.Getenv("DATABRICKS_LOG_LEVEL") == "" {
		_ = dbsqllog.SetLogLevel("disabled")
		sdklog.DefaultLogger = &sdklog.SimpleLogger{Level: sdklog.LevelError + 1}
	}
}

func connectionFields() []plugin.Field {
	return []plugin.Field{
		{Name: "host", Description: "workspace host, e.g. adb-123.4.azuredatabricks.net", Required: true},
		{Name: "http_path", Description: "the SQL warehouse's HTTP path, e.g. /sql/1.0/warehouses/abc", Required: true},
		{Name: "auth_type", Description: "pat (a token) or oauth (browser sign-in; with client_id and client_secret, a service principal)", Default: "pat", SameAsSource: "databricks"},
		{Name: "token", Description: "personal access token, for auth_type pat", Secret: true, SameAsSource: "databricks"},
		{Name: "client_id", Description: "OAuth client; a service principal's application ID (browser sign-in defaults to databricks-cli)", SameAsSource: "databricks"},
		{Name: "client_secret", Description: "service principal OAuth secret", Secret: true, SameAsSource: "databricks"},
		{Name: "catalog", Description: "default catalog"},
		{Name: "schema", Description: "default schema"},
	}
}
