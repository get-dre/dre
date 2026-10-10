// Command dre-plugin-bigquery is DRE's BigQuery plugin package: the `bigquery` source, on
// Google's official Go client. It speaks DRE's plugin protocol (docs/protocol.md) on stdin and
// stdout through the shared Go module, and logs to stderr.
//
// Profile fields follow dbt-bigquery's names and values, so a dbt profile can be copied across:
// `method` (oauth, oauth-secrets, service-account, service-account-json, external-oauth-wif),
// `project` (or dbt's `database`), `dataset` (or `schema`), `location`, the sign-in fields of each
// method, `impersonate_service_account`, `scopes`, `execution_project`, `quota_project`,
// `priority`, `maximum_bytes_billed`, the job timeout and retry fields, and `api_endpoint`.
// Fields that only matter for building models (`threads`, Dataproc, `gcs_bucket`, ...) are
// accepted and ignored.
//
// Every statement of a Binding runs in one BigQuery session, created on `open`, so temp tables
// from one query are there for the next.
package main

import (
	"github.com/get-dre/dre/go/plugin"
)

// version is set at build time with -ldflags "-X main.version=<version>".
var version = "unreleased"

var (
	sourceRole = plugin.Role{
		Kind: "source", Name: "bigquery", Capabilities: []string{"sessions", "check", "load", "validate"},
		Fields: fields, IdentifierQuote: "`",
		Open: func(conn map[string]any) (plugin.Session, error) { return open(conn) },
	}
	pkg = plugin.Package{Version: version, Roles: []plugin.Role{sourceRole}}
)

func main() { plugin.Main(pkg) }

var fields = []plugin.Field{
	{Name: "method", Description: "how to sign in: oauth (gcloud application-default credentials), service-account (a key file), service-account-json, oauth-secrets or external-oauth-wif", Default: "oauth"},
	{Name: "project", Description: "the Google Cloud project to read from (dbt's `database` also works)", Required: true},
	{Name: "dataset", Description: "default dataset for unqualified table names (dbt's `schema` also works)"},
	{Name: "location", Description: "where jobs run, e.g. US, EU or europe-west2"},
	{Name: "keyfile", Description: "path to a service account key file, for method service-account"},
	{Name: "keyfile_json", Description: "a service account key's JSON, inline, for method service-account-json", Secret: true, Manual: true},
	{Name: "token", Description: "an OAuth access token, for method oauth-secrets", Secret: true, Manual: true},
	{Name: "refresh_token", Description: "an OAuth refresh token, for method oauth-secrets with client_id and client_secret", Secret: true, Manual: true},
	{Name: "client_id", Description: "OAuth client ID, for method oauth-secrets with a refresh token", Manual: true},
	{Name: "client_secret", Description: "OAuth client secret, for method oauth-secrets with a refresh token", Secret: true, Manual: true},
	{Name: "token_uri", Description: "where a refresh token is exchanged (default https://oauth2.googleapis.com/token)", Manual: true},
	{Name: "workload_pool_provider_path", Description: "the workload identity pool provider, for method external-oauth-wif", Manual: true},
	{Name: "token_endpoint", Description: "where the external identity's token comes from, for method external-oauth-wif: type, request_url, request_data", Manual: true},
	{Name: "service_account_impersonation_url", Description: "the service account to impersonate after workload identity federation", Manual: true},
	{Name: "impersonate_service_account", Description: "a service account email to run as, using the signed-in identity", Manual: true},
	{Name: "scopes", Description: "OAuth scopes (default: bigquery, cloud-platform and drive)", Manual: true},
	{Name: "execution_project", Description: "the project jobs run and are billed in, when not `project`", Manual: true},
	{Name: "quota_project", Description: "the project API quota is charged to", Manual: true},
	{Name: "priority", Description: "interactive (default) or batch", Manual: true},
	{Name: "maximum_bytes_billed", Description: "a job that would bill more bytes than this fails instead of running", Manual: true},
	{Name: "job_execution_timeout_seconds", Description: "stop a query that runs longer than this", Manual: true},
	{Name: "job_creation_timeout_seconds", Description: "give up starting a query after this long", Manual: true},
	{Name: "retries", Description: "how many times to try starting the session again after a temporary error (0: never)", Default: 3, Manual: true},
	{Name: "job_retries", Description: "how many times a query that fails with a transient error is run again (default 1)", Manual: true},
	{Name: "job_retry_deadline_seconds", Description: "stop retrying a query after this long", Manual: true},
	{Name: "api_endpoint", Description: "a BigQuery API endpoint other than Google's, e.g. Private Service Connect or an emulator", Manual: true},
}

// aliases are dbt's other names for fields, and dbtOnly the dbt fields that only matter for
// building models: both accepted so a copied dbt profile works.
var (
	aliases = []string{"database", "schema", "timeout_seconds"}
	dbtOnly = []string{
		"threads", "dataproc_region", "dataproc_cluster_name", "dataproc_batch", "gcs_bucket", "compute_region",
		"submission_method", "reuse_connections",
	}
)
