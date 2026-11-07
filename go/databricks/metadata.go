package main

import "github.com/get-dre/dre/go/plugin"

func sourceDialect() *plugin.SQLDialect {
	hex := "decode(unhex('{hex}'), 'UTF-8')"
	notice := "SQL validation; use a read-only Databricks database role"
	return &plugin.SQLDialect{
		Parser: "databricks", IdentifierQuote: "`", IdentifierCase: "insensitive",
		LiteralStyle: "hex", HexLiteral: &hex,
		DeniedFunctions: []string{"read_files", "read_kafka", "read_kinesis", "read_pubsub", "read_pulsar", "read_state_metadata", "read_statestore", "cloud_files_state", "http_request", "java_method", "reflect", "secret", "try_secret", "list_secrets", "ai_query"},
		FileFormats:     []string{"parquet", "csv", "json", "text", "delta", "orc", "avro", "binaryFile", "xml"},
		ReadOnlyNotice:  &notice,
	}
}
