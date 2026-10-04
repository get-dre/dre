// Command dre-plugin-snowflake is DRE's Snowflake plugin package: the `snowflake` source, on
// Snowflake's official Go driver (gosnowflake). It speaks DRE's plugin protocol
// (docs/protocol.md) on stdin and stdout through the shared Go module, and logs to stderr.
//
// Profile fields follow dbt-snowflake's names and values, so a dbt profile can be copied across:
// account, user, role, warehouse, database, schema, authenticator and the fields of each sign-in
// method, query_tag, and the connect and retry fields. Fields that only matter for building
// models are accepted and ignored.
//
// One connection is one Snowflake session, held for the whole Binding. The driver keeps the SSO
// and MFA token cache, as it does for dbt; DRE writes nothing of its own for Snowflake.
package main

import (
	"github.com/get-dre/dre/go/plugin"
)

// version is set at build time with -ldflags "-X main.version=<version>".
var version = "unreleased"

var (
	sourceRole = plugin.Role{
		Kind: "source", Name: "snowflake", Capabilities: []string{"sessions", "check", "load", "validate"},
		Fields: fields, IdentifierQuote: `"`,
		Open: func(conn map[string]any) (plugin.Session, error) { return open(conn) },
	}
	pkg = plugin.Package{Version: version, Roles: []plugin.Role{sourceRole}}
)

func main() { plugin.Main(pkg) }

var fields = []plugin.Field{
	{Name: "account", Description: "account identifier, e.g. myorg-myaccount (or a locator like xy12345.eu-west-1)", Required: true},
	{Name: "user", Description: "the user to sign in as", Required: true},
	{Name: "authenticator", Description: "how to sign in: snowflake (password, the default), username_password_mfa, externalbrowser, oauth, jwt, programmatic_access_token, workload_identity, or an Okta URL; a private key means key-pair sign-in"},
	{Name: "password", Description: "password (or a programmatic access token)", Secret: true},
	{Name: "private_key_path", Description: "path to a key-pair private key (PEM), for key-pair sign-in"},
	{Name: "private_key", Description: "a key-pair private key, inline (PEM, or base64 DER)", Secret: true, Manual: true},
	{Name: "private_key_passphrase", Description: "the private key's passphrase, if it's encrypted", Secret: true},
	{Name: "role", Description: "role to use"},
	{Name: "warehouse", Description: "warehouse to run queries on"},
	{Name: "database", Description: "default database"},
	{Name: "schema", Description: "default schema"},
	{Name: "token", Description: "OAuth access token, JWT, programmatic access token, or OIDC token for workload identity", Secret: true, Manual: true},
	{Name: "oauth_client_id", Description: "OAuth client ID, to renew `token` as a refresh token", Manual: true},
	{Name: "oauth_client_secret", Description: "OAuth client secret, to renew `token` as a refresh token", Secret: true, Manual: true},
	{Name: "workload_identity_provider", Description: "for workload_identity: AWS, AZURE, GCP or OIDC", Manual: true},
	{Name: "workload_identity_entra_resource", Description: "for workload_identity on Azure: the Entra resource", Manual: true},
	{Name: "query_tag", Description: "tag for every query of the session", Manual: true},
	{Name: "client_session_keep_alive", Description: "keep the session alive while a long report runs", Manual: true},
	{Name: "client_request_mfa_token", Description: "cache the MFA token (username_password_mfa) in the OS keychain", Manual: true},
	{Name: "client_store_temporary_credential", Description: "cache the SSO token (externalbrowser) in the OS keychain", Manual: true},
	{Name: "connect_retries", Description: "how many times to retry connecting (default 1)", Manual: true},
	{Name: "connect_timeout", Description: "seconds to wait for a connection (default 10)", Manual: true},
	{Name: "host", Description: "Snowflake host, when not <account>.snowflakecomputing.com", Manual: true},
	{Name: "port", Description: "port, with host", Manual: true},
	{Name: "protocol", Description: "https (default) or http, with host", Manual: true},
	{Name: "insecure_mode", Description: "skip the certificate revocation check", Manual: true},
	{Name: "proxy_host", Description: "HTTP proxy host", Manual: true},
	{Name: "proxy_port", Description: "HTTP proxy port", Manual: true},
}

// dbtOnly are dbt-snowflake fields that only matter for building models or that DRE handles
// itself: accepted so a copied dbt profile works.
var dbtOnly = []string{"threads", "reuse_connections", "retry_on_database_errors", "retry_all", "s3_stage_vpce_dns_name"}
