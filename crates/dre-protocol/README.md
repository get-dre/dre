# dre-protocol

The plugin SDK of [DRE](https://github.com/get-dre/dre), the Declarative Reporting Engine:
frames, messages and helpers for writing a plugin in Rust. Process management and the Rust
conformance runner are in `dre-plugin-host`; language-neutral conformance fixtures remain here.

Every DRE source, format and destination is a separate program speaking this protocol over
stdin/stdout, in any language. See the
[protocol specification](https://github.com/get-dre/dre/blob/master/docs/protocol.md).

The package version and negotiated integer wire version are independent. Optional fields may
be added within a wire version when absence preserves existing behavior. New exchanges require
capability negotiation or explicit opt-in; incompatible changes require a new wire version.
`Request` and `Response` are non-exhaustive: downstream Rust matches need a wildcard.
