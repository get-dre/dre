//! DRE destination plugin `sftp`.
//!
//! Profile target fields: `host`, `port` (22), `username`, and `password`, `private_key_path` or
//! `private_key` (the key's text; + `private_key_passphrase`). The server's host key must be
//! trusted: it's checked against `known_hosts_path` (default `~/.ssh/known_hosts`) or a pinned
//! `host_key_fingerprint` (`SHA256:...`, as `ssh-keygen -lf` prints it). Unknown hosts are
//! refused unless `accept_unknown_host: true`. Missing remote directories are created. The SSH
//! settings and checks are shared with the postgres tunnel (`dre-ssh`). `connect_timeout` (30s)
//! and `timeout` (60s without progress) bound the connection.

use std::path::Path;

use dre_protocol::delivery::{Rules, timeout_fields};
use dre_protocol::msg::ConnectionField;
use dre_protocol::plugin::{About, Destination, Result, conn_bool, serve_destination};
use dre_ssh::Ssh;
use dre_ssh::russh::{self, client};
use russh_sftp::client::SftpSession;
use serde_json::{Map, Value};
use tokio::io::AsyncWriteExt;

struct Sftp;

async fn upload(local: &Path, remote: &str, c: &Map<String, Value>) -> Result<String> {
    let mut ssh = Ssh::from_settings(c, "")?;
    ssh.accept_unknown = conn_bool(c, "accept_unknown_host").unwrap_or(false);
    let (rules, _) = Rules::from_settings(Rules::default(), c, &Map::new(), &[])?;
    let config = client::Config {
        inactivity_timeout: Some(rules.timeout),
        ..Default::default()
    };
    let session = ssh.connect(config, rules.connect_timeout).await?;
    let (user, host, port) = (&ssh.username, &ssh.host, ssh.port);
    let channel = session.channel_open_session().await?;
    channel.request_subsystem(true, "sftp").await?;
    let sftp = SftpSession::new(channel.into_stream())
        .await
        .map_err(|e| format!("can't start SFTP: {e}"))?;

    // Create missing parent directories.
    let mut dir = String::new();
    let parts: Vec<&str> = remote.split('/').collect();
    for (i, part) in parts[..parts.len().saturating_sub(1)].iter().enumerate() {
        if part.is_empty() {
            if i == 0 {
                dir.push('/');
            }
            continue;
        }
        if !dir.is_empty() && !dir.ends_with('/') {
            dir.push('/');
        }
        dir.push_str(part);
        if !sftp.try_exists(dir.clone()).await.unwrap_or(false) {
            sftp.create_dir(dir.clone())
                .await
                .map_err(|e| format!("can't create directory {dir}: {e}"))?;
        }
    }
    let mut src = tokio::fs::File::open(local)
        .await
        .map_err(|e| format!("can't read {}: {e}", local.display()))?;
    let mut dst = sftp
        .create(remote)
        .await
        .map_err(|e| format!("can't create {remote}: {e}"))?;
    tokio::io::copy(&mut src, &mut dst)
        .await
        .map_err(|e| format!("upload to {remote} failed: {e}"))?;
    dst.shutdown()
        .await
        .map_err(|e| format!("upload to {remote} failed: {e}"))?;
    let _ = sftp.close().await;
    let _ = session
        .disconnect(russh::Disconnect::ByApplication, "", "en")
        .await;
    Ok(format!(
        "sftp://{user}@{host}:{port}/{}",
        remote.trim_start_matches('/')
    ))
}

impl Destination for Sftp {
    fn connection_fields(&self) -> Vec<ConnectionField> {
        vec![
            ConnectionField::new("host", "SFTP server").required(),
            ConnectionField::new("port", "port").default(22),
            ConnectionField::new("username", "user name").required(),
        ]
        .into_iter()
        .chain(dre_ssh::auth_fields())
        .chain(timeout_fields())
        .collect()
    }

    fn deliver(&mut self, local: &Path, remote: Option<&str>, c: &Map<String, Value>) -> Result<String> {
        let remote = remote.ok_or("sftp needs `output.destination.path`")?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(upload(local, remote, c))
    }
}

fn main() {
    serve_destination(About::new("sftp", env!("CARGO_PKG_VERSION")), Sftp)
}
