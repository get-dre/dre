# dre-plugin-host

Process spawning, requests and conformance checks for the DRE plugin protocol.
Plugin implementations depend on `dre-protocol`; hosts and plugin tests use this crate.
The Rust API is internal to the matching DRE release and carries no stability guarantee.
The wire contract lives in [the protocol specification](https://github.com/get-dre/dre/blob/master/docs/protocol.md).
