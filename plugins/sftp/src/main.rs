//! DRE destination plugin `sftp`.
//!
//! Profile target fields: `host`, `port` (22), `username`, and `password`, `private_key_path` or
//! `private_key` (the key's text; + `private_key_passphrase`). The server's host key must be
//! trusted: it's checked against `known_hosts_path` (default `~/.ssh/known_hosts`) or a pinned
//! `host_key_fingerprint` (`SHA256:...`, as `ssh-keygen -lf` prints it). Unknown hosts are
//! refused unless `accept_unknown_host: true`. Missing remote directories are created. The SSH
//! settings and checks are shared with the postgres tunnel (`dre-ssh`). `connect_timeout` (30s)
//! and `timeout` (60s without progress) bound the connection.
//!
//! Destination options: `atomic` (default true: upload as `.<name>.dre-part`, then rename, so a
//! dropped connection never leaves a half-written file under the final name) and `temp_dir`
//! (where that temporary file goes, on the same server).

use std::path::Path;

use dre_protocol::delivery::{self, Caps, Rules, Store, StoreError, deliver, timeout_fields};
use dre_protocol::msg::ConnectionField;
use dre_protocol::options::OptionField;
use dre_protocol::plugin::{About, Delivery, Destination, PluginError, Result, conn_bool, serve_destination};
use dre_ssh::Ssh;
use dre_ssh::russh::{self, client};
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::OpenFlags;
use serde_json::{Map, Value};
use tokio::io::AsyncWriteExt;

struct Sftp;

/// One SFTP session, as the shared delivery rules' store.
struct SftpStore<'a> {
    rt: &'a tokio::runtime::Runtime,
    sftp: SftpSession,
}

fn failed(what: &str, path: &str, e: impl std::fmt::Display) -> StoreError {
    StoreError::Failed(format!("can't {what} {path}: {e}"))
}

impl SftpStore<'_> {
    /// Create `path`'s missing parent folders.
    async fn make_parents(&self, path: &str) -> std::result::Result<(), StoreError> {
        let mut dir = String::new();
        let parts: Vec<&str> = path.split('/').collect();
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
            if !self.sftp.try_exists(dir.clone()).await.unwrap_or(false) {
                self.sftp
                    .create_dir(dir.clone())
                    .await
                    .map_err(|e| failed("create directory", &dir, e))?;
            }
        }
        Ok(())
    }
}

impl Store for SftpStore<'_> {
    fn caps(&self) -> Caps {
        // An exclusive create in one step; a plain SFTP rename refuses to replace a file.
        Caps {
            create_exclusive: true,
            rename_no_replace: true,
            visible_when_complete: false,
        }
    }

    fn write(&mut self, local: &Path, remote: &str, exclusive: bool) -> std::result::Result<(), StoreError> {
        self.rt.block_on(async {
            self.make_parents(remote).await?;
            let mut src = tokio::fs::File::open(local)
                .await
                .map_err(|e| StoreError::Failed(format!("can't read {}: {e}", local.display())))?;
            let flags = if exclusive {
                OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE
            } else {
                OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE
            };
            let mut dst = match self.sftp.open_with_flags(remote, flags).await {
                Ok(f) => f,
                Err(_) if exclusive && self.sftp.try_exists(remote).await.unwrap_or(false) => {
                    return Err(StoreError::Exists);
                }
                Err(e) => return Err(failed("create", remote, e)),
            };
            tokio::io::copy(&mut src, &mut dst)
                .await
                .map_err(|e| failed("upload to", remote, e))?;
            dst.shutdown().await.map_err(|e| failed("upload to", remote, e))
        })
    }

    fn rename(&mut self, from: &str, to: &str, replace: bool) -> std::result::Result<(), StoreError> {
        self.rt.block_on(async {
            self.make_parents(to).await?;
            match self.sftp.rename(from, to).await {
                Ok(()) => Ok(()),
                Err(e) => {
                    if !self.sftp.try_exists(to).await.unwrap_or(false) {
                        return Err(failed("rename to", to, e));
                    }
                    if !replace {
                        return Err(StoreError::Exists);
                    }
                    // SFTP's rename won't replace: remove the old file, then rename (a short
                    // window with no file at `to`).
                    self.sftp.remove_file(to).await.map_err(|e| failed("replace", to, e))?;
                    self.sftp.rename(from, to).await.map_err(|e| failed("rename to", to, e))
                }
            }
        })
    }

    fn exists(&mut self, remote: &str) -> std::result::Result<bool, StoreError> {
        self.rt
            .block_on(self.sftp.try_exists(remote))
            .map_err(|e| failed("look for", remote, e))
    }

    fn delete(&mut self, remote: &str) -> std::result::Result<(), StoreError> {
        self.rt.block_on(async {
            if self.sftp.try_exists(remote).await.unwrap_or(false) {
                self.sftp.remove_file(remote).await.map_err(|e| failed("remove", remote, e))?;
            }
            Ok(())
        })
    }
}

fn upload(local: &Path, remote: &str, c: &Map<String, Value>, options: &Map<String, Value>) -> Result<String> {
    let mut ssh = Ssh::from_settings(c, "")?;
    ssh.accept_unknown = conn_bool(c, "accept_unknown_host").unwrap_or(false);
    let (rules, _) = Rules::from_settings(Rules::default(), c, options, &[])?;
    let config = client::Config {
        inactivity_timeout: Some(rules.timeout),
        ..Default::default()
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let (session, sftp) = rt.block_on(async {
        let session = ssh.connect(config, rules.connect_timeout).await?;
        let channel = session.channel_open_session().await?;
        channel.request_subsystem(true, "sftp").await?;
        let sftp = SftpSession::new(channel.into_stream())
            .await
            .map_err(|e| format!("can't start SFTP: {e}"))?;
        Ok::<_, dre_protocol::plugin::Error>((session, sftp))
    })?;
    let (user, host, port) = (&ssh.username, &ssh.host, ssh.port);
    let mut store = SftpStore { rt: &rt, sftp };
    let delivered = deliver(&mut store, local, remote, &rules).map_err(PluginError::from)?;
    rt.block_on(async {
        let _ = store.sftp.close().await;
        let _ = session.disconnect(russh::Disconnect::ByApplication, "", "en").await;
    });
    Ok(format!(
        "sftp://{user}@{host}:{port}/{}",
        delivered.path.trim_start_matches('/')
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

    /// `atomic` and `temp_dir` (the shared delivery rules).
    fn options(&self) -> Vec<OptionField> {
        delivery::option_fields()
            .into_iter()
            .filter(|f| f.name == "atomic" || f.name == "temp_dir")
            .collect()
    }

    fn deliver_files(&mut self, d: &Delivery) -> Result<String> {
        let [f] = d.files.as_slice() else {
            return Err("sftp takes one file per delivery".into());
        };
        let remote = f.remote.as_deref().ok_or("sftp needs `output.destination.path`")?;
        upload(&f.local, remote, &d.connection, &d.options)
    }
}

fn main() {
    serve_destination(About::new("sftp", env!("CARGO_PKG_VERSION")), Sftp)
}
