module github.com/get-dre/dre/go/clickhouse

go 1.27

require (
	github.com/ClickHouse/clickhouse-go/v2 v2.49.0
	github.com/apache/arrow-go/v18 v18.8.0
	github.com/get-dre/dre/go/plugin v0.0.0
	github.com/google/uuid v1.6.0
	github.com/paulmach/orb v0.13.0
	github.com/shopspring/decimal v1.4.0
	golang.org/x/crypto v0.56.0
)

require (
	github.com/ClickHouse/ch-go v0.74.0 // indirect
	github.com/andybalholm/brotli v1.2.6 // indirect
	github.com/cespare/xxhash/v2 v2.3.0 // indirect
	github.com/go-faster/city v1.0.1 // indirect
	github.com/go-faster/errors v0.7.1 // indirect
	github.com/goccy/go-json v0.10.6 // indirect
	github.com/google/flatbuffers v25.12.19+incompatible // indirect
	github.com/klauspost/compress v1.19.2 // indirect
	github.com/klauspost/cpuid/v2 v2.4.0 // indirect
	github.com/pierrec/lz4/v4 v4.1.29 // indirect
	github.com/segmentio/asm v1.2.1 // indirect
	github.com/zeebo/xxh3 v1.1.0 // indirect
	go.opentelemetry.io/otel v1.46.0 // indirect
	go.opentelemetry.io/otel/trace v1.46.0 // indirect
	golang.org/x/exp v0.0.0-20260112195511-716be5621a96 // indirect
	golang.org/x/sys v0.47.0 // indirect
)

replace github.com/get-dre/dre/go/plugin => ../plugin
