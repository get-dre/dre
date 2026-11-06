# dre-core

The engine of [DRE](https://github.com/get-dre/dre), the Declarative Reporting Engine: project
parsing and validation, Jinja rendering, the plugin manager and the run path. The `dre` command
is in [`dre-cli`](https://crates.io/crates/dre-cli); install it with `cargo install dre-cli`.

This is an implementation library for the matching CLI release. Its Rust API has no stability
guarantee; engine internals are private. Use the CLI and its published JSON contracts for
integrations, or `dre-protocol` for plugins.
