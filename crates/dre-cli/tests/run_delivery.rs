//! Delivery through a destination plugin: a failure keeps the output in target/.

mod common;

use common::TestProject;

#[test]
fn a_failed_upload_leaves_the_output_in_target_and_says_so() {
    // Nothing listens on this port, so the SFTP connection fails.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let profiles = format!(
        "connections:\n  warehouse:\n    targets:\n      dev: {{type: duckdb, path: data.duckdb}}\n\
         destinations:\n  client_sftp:\n    targets:\n      dev: {{type: sftp, host: 127.0.0.1, port: {port}, username: dre, password: x}}\n"
    );
    let p = TestProject::new(
        &[
            (
                "dre_project.yml",
                "name: acme_reports\ndefault_profile: warehouse\n",
            ),
            ("dependencies.yml", "plugins: [duckdb, csv, sftp]\n"),
            (
                "reports/ops/daily/daily.yml",
                "queries: [q]\noutput:\n  destination: {profile: client_sftp, path: /inbound/daily.csv}\n",
            ),
            ("reports/ops/daily/q.sql", "select 1 as n"),
        ],
        &profiles,
    );
    p.duckdb("data.duckdb", "select 1;");
    p.dre("run", &["daily"])
        .failed()
        .says("delivery through `sftp` failed")
        .says("the output is still in target/");
    assert_eq!(p.read("target/run/daily/default/daily.csv"), "n\r\n1\r\n");
    assert_eq!(
        p.json("target/run/daily/default/run_results.json")["status"],
        "failed"
    );
}

#[test]
fn env_var_booleans_reach_the_plugin_as_booleans() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let profiles = "connections:\n  warehouse:\n    targets:\n      dev: {type: duckdb, path: data.duckdb}\n\
         destinations:\n  client_sftp:\n    targets:\n      dev: {type: sftp, host: 127.0.0.1, port: \"{{ env_var('SFTP_PORT') }}\", username: dre, password: x,\n\
         \x20       accept_unknown_host: \"{{ env_var('SFTP_ACCEPT', 'false') }}\", use_agent: \"{{ env_var('SFTP_AGENT', 'no') }}\"}\n";
    let p = TestProject::new(
        &[
            (
                "dre_project.yml",
                "name: acme_reports\ndefault_profile: warehouse\n",
            ),
            ("dependencies.yml", "plugins: [duckdb, csv, sftp]\n"),
            (
                "reports/ops/daily/daily.yml",
                "queries: [q]\noutput:\n  destination: {profile: client_sftp, path: /inbound/daily.csv}\n",
            ),
            ("reports/ops/daily/q.sql", "select 1 as n"),
        ],
        profiles,
    );
    p.duckdb("data.duckdb", "select 1;");
    let port = port.to_string();
    let env = [("SFTP_PORT", port.as_str()), ("SFTP_ACCEPT", "true")];
    p.dre_env("validate", &[], &env)
        .ok()
        .lacks("must be true or false");
    // Nothing listens on the port: the run gets as far as connecting.
    p.dre_env("run", &["daily"], &env)
        .failed()
        .says("delivery through `sftp` failed")
        .lacks("must be true or false");
    // A value that isn't a boolean is still refused.
    p.dre_env(
        "validate",
        &[],
        &[("SFTP_PORT", port.as_str()), ("SFTP_ACCEPT", "maybe")],
    )
    .failed()
    .says("`accept_unknown_host` must be true or false");
}
