//! SSH connections for DRE plugins: the `sftp` destination and the `postgres` source's bastion
//! tunnel share one set of settings and rules.
//!
//! Settings: `host`, `port` (22), `username`, and `password`, `private_key_path` or `private_key`
//! (the key's text, e.g. from `env_var()`; a literal `\n` counts as a line break), with
//! `private_key_passphrase` for an encrypted key. The server's host key must be trusted: it's
//! checked against `known_hosts_path` (default `~/.ssh/known_hosts`) or a pinned
//! `host_key_fingerprint` (`SHA256:...`, as `ssh-keygen -lf` prints it).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dre_protocol::delivery::{Retry, is_connection_error};
use dre_protocol::msg::{ConnectionField, FieldKind};
use dre_protocol::plugin::{Result, conn_str};
use russh::client;
use russh::keys::{HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKey};
use serde_json::{Map, Value};

pub use russh;

/// An authenticated SSH session.
pub type Session = client::Handle<HostCheck>;

enum Auth {
    Key(Arc<PrivateKey>),
    Password(String),
    /// `use_agent: true`: the SSH agent signs (`SSH_AUTH_SOCK`), so the key never enters DRE.
    Agent,
}

/// RSA keys sign with the `rsa` crate, which has a timing side channel (RUSTSEC-2023-0071) and no
/// fixed release yet. The advice, for the warning and the refusal.
const RSA_ADVICE: &str = "RSA private keys use code with a known timing weakness (RUSTSEC-2023-0071); \
the practical risk is low, as DRE signs only once per connection. To move off RSA, make an Ed25519 \
key (`ssh-keygen -t ed25519`) and add its public key on the server, or sign through your SSH agent \
with `use_agent: true`. `allow_rsa_keys: true` keeps the RSA key without this warning";

/// Whether the RSA warning has been given (once per run of the plugin).
static RSA_WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Refuse an RSA key with `allow_rsa_keys: false`; warn once when it's unset.
fn check_rsa(key: &PrivateKey, allow: Option<bool>, prefix: &str) -> Result<()> {
    if !matches!(key.algorithm(), russh::keys::Algorithm::Rsa { .. }) {
        return Ok(());
    }
    match allow {
        Some(false) => Err(format!(
            "the private key is an RSA key, and `{prefix}allow_rsa_keys: false` refuses RSA keys. \
             {RSA_ADVICE}"
        )
        .into()),
        Some(true) => {
            dre_protocol::log::debug!("using an RSA private key (`allow_rsa_keys: true`)");
            Ok(())
        }
        None => {
            if !RSA_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                dre_protocol::log::warn!("{RSA_ADVICE}.");
            }
            Ok(())
        }
    }
}

/// Where to connect, as whom, and which host key to trust.
pub struct Ssh {
    pub host: String,
    pub port: u16,
    pub username: String,
    auth: Auth,
    pub known_hosts: PathBuf,
    pub fingerprint: Option<String>,
    /// Trust a host missing from `known_hosts` (sftp's `accept_unknown_host`). A key that
    /// contradicts `known_hosts` is refused regardless.
    pub accept_unknown: bool,
}

/// The authentication and host-key fields, as a plugin declares them after `host`, `port` and
/// `username`.
pub fn auth_fields() -> Vec<ConnectionField> {
    vec![
        ConnectionField::new("password", "password (or set private_key_path)").secret(),
        ConnectionField::new("private_key_path", "private key file (instead of a password)")
            .kind(FieldKind::Path),
        ConnectionField::new("private_key_passphrase", "the private key's passphrase")
            .secret()
            .manual(),
        ConnectionField::new(
            "known_hosts_path",
            "known_hosts file (default ~/.ssh/known_hosts)",
        )
        .kind(FieldKind::Path)
        .manual(),
        ConnectionField::new(
            "private_key",
            "the private key's text, e.g. from env_var() (instead of private_key_path)",
        )
        .secret()
        .manual(),
        ConnectionField::new(
            "host_key_fingerprint",
            "pinned host key, SHA256:... (otherwise ~/.ssh/known_hosts is used)",
        ),
        ConnectionField::new(
            "use_agent",
            "sign in with the keys in your SSH agent (SSH_AUTH_SOCK), instead of a password or key file",
        )
        .kind(FieldKind::Boolean)
        .manual(),
        ConnectionField::new(
            "allow_rsa_keys",
            "false refuses RSA private keys; true uses them without the RUSTSEC-2023-0071 warning",
        )
        .kind(FieldKind::Boolean)
        .manual(),
    ]
}

/// A path from a profile, with a leading `~/` meaning the home directory (profiles aren't run
/// through a shell, so nothing else expands it).
pub fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        Some(rest) => std::env::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(path),
    }
}

/// A private key's text as it arrives from an environment variable: a secret stored on one line
/// with literal `\n` sequences gets its line breaks back.
pub fn key_text(s: &str) -> String {
    if s.contains('\n') {
        s.to_string()
    } else {
        s.replace("\\r\\n", "\n").replace("\\n", "\n")
    }
}

impl Ssh {
    /// Read the settings from a profile's fields; `prefix` names where they live in error
    /// messages (`""` at the top level, `"ssh."` for a nested block).
    pub fn from_settings(c: &Map<String, Value>, prefix: &str) -> Result<Ssh> {
        let required = |key: &str| {
            conn_str(c, key)
                .map(str::to_string)
                .ok_or_else(|| format!("the profile output needs a `{prefix}{key}` field"))
        };
        let host = required("host")?;
        let port: u16 = match c.get("port") {
            Some(Value::Number(n)) => n
                .as_u64()
                .and_then(|p| u16::try_from(p).ok())
                .ok_or_else(|| format!("invalid `{prefix}port` `{n}`"))?,
            Some(Value::String(s)) => s.parse().map_err(|_| format!("invalid `{prefix}port` `{s}`"))?,
            _ => 22,
        };
        let username = required("username")?;
        let passphrase = conn_str(c, "private_key_passphrase");
        let flag = |key: &str| match c.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(v) => Err(format!("`{prefix}{key}` must be true or false, got {v}")),
        };
        let allow_rsa = flag("allow_rsa_keys")?;
        let use_agent = flag("use_agent")?.unwrap_or(false);
        let key_given = conn_str(c, "private_key_path").is_some() || conn_str(c, "private_key").is_some();
        if use_agent && key_given {
            return Err(format!(
                "set `{prefix}use_agent` or a private key (`{prefix}private_key_path`, `{prefix}private_key`), not both"
            )
            .into());
        }
        let auth = match (conn_str(c, "private_key_path"), conn_str(c, "private_key")) {
            _ if use_agent => Auth::Agent,
            (Some(_), Some(_)) => {
                return Err(
                    format!("set `{prefix}private_key_path` or `{prefix}private_key`, not both").into(),
                );
            }
            (Some(path), None) => Auth::Key(Arc::new(
                russh::keys::load_secret_key(expand_home(path), passphrase)
                    .map_err(|e| format!("can't load private key {path}: {e}"))?,
            )),
            (None, Some(text)) => Auth::Key(Arc::new(
                russh::keys::decode_secret_key(&key_text(text), passphrase)
                    .map_err(|e| format!("can't read `{prefix}private_key`: {e}"))?,
            )),
            (None, None) => match conn_str(c, "password") {
                Some(pw) => Auth::Password(pw.to_string()),
                None => {
                    return Err(format!(
                        "set `{prefix}password`, `{prefix}private_key_path`, `{prefix}private_key` or `{prefix}use_agent: true`"
                    )
                    .into());
                }
            },
        };
        if let Auth::Key(key) = &auth {
            check_rsa(key, allow_rsa, prefix)?;
        }
        Ok(Ssh {
            host,
            port,
            username,
            auth,
            known_hosts: conn_str(c, "known_hosts_path")
                .map(expand_home)
                .unwrap_or_else(home_known_hosts),
            fingerprint: conn_str(c, "host_key_fingerprint").map(str::to_string),
            accept_unknown: false,
        })
    }

    /// Connect, check the host key and authenticate. `timeout` bounds reaching the server and
    /// the key exchange.
    pub async fn connect(&self, config: client::Config, timeout: Duration) -> Result<Session> {
        self.try_connect(config, timeout).await.map_err(|e| match e {
            Retry::Temporary(e, _) | Retry::Fail(e) => e,
        })
    }

    /// [`connect`](Self::connect), saying whether trying again may help: a timeout or a
    /// connection refused or dropped may; refused credentials or a host key that doesn't match
    /// won't.
    pub async fn try_connect(
        &self,
        config: client::Config,
        timeout: Duration,
    ) -> std::result::Result<Session, Retry<dre_protocol::plugin::Error>> {
        let (host, port) = (self.host.as_str(), self.port);
        let refused = Arc::new(Mutex::new(None));
        let check = HostCheck {
            host: host.to_string(),
            port,
            known_hosts: self.known_hosts.clone(),
            fingerprint: self.fingerprint.clone(),
            accept_unknown: self.accept_unknown,
            refused: refused.clone(),
        };
        let connecting = client::connect(Arc::new(config), (host, port), check);
        let temporary = |m: String| Retry::Temporary(m.into(), None);
        let fail = |m: String| Retry::Fail(m.into());
        let mut session = match tokio::time::timeout(timeout, connecting).await {
            Err(_) => return Err(temporary(format!("timed out connecting to {host}:{port}"))),
            Ok(Err(e)) => {
                if let Some(why) = refused.lock().unwrap().take() {
                    return Err(fail(why));
                }
                let m = format!("can't connect to {host}:{port}: {e}");
                return Err(match &e {
                    russh::Error::IO(io) if is_connection_error(io) => temporary(m),
                    russh::Error::Disconnect | russh::Error::ConnectionTimeout => temporary(m),
                    _ => fail(m),
                });
            }
            Ok(Ok(s)) => s,
        };
        let user = &self.username;
        let dropped = |e: russh::Error| Retry::Temporary(e.into(), None);
        let auth = match &self.auth {
            Auth::Key(key) => {
                let hash = session
                    .best_supported_rsa_hash()
                    .await
                    .map_err(dropped)?
                    .flatten();
                session
                    .authenticate_publickey(user, PrivateKeyWithHashAlg::new(key.clone(), hash))
                    .await
                    .map_err(dropped)?
            }
            Auth::Password(pw) => session.authenticate_password(user, pw).await.map_err(dropped)?,
            Auth::Agent => {
                if agent_sign_in(&mut session, user).await.map_err(fail)? {
                    return Ok(session);
                }
                return Err(fail(format!(
                    "{host}:{port} accepted none of the SSH agent's keys for `{user}`"
                )));
            }
        };
        if !auth.success() {
            return Err(fail(format!(
                "{host}:{port} refused the credentials for `{user}`"
            )));
        }
        Ok(session)
    }
}

/// Sign in with each of the SSH agent's keys in turn; whether one was accepted. The agent
/// signs, so the private key (RSA included) never enters DRE.
async fn agent_sign_in(session: &mut Session, user: &str) -> std::result::Result<bool, String> {
    #[cfg(unix)]
    let mut agent = russh::keys::agent::client::AgentClient::connect_env()
        .await
        .map_err(|e| format!("can't reach the SSH agent (`use_agent: true`; is SSH_AUTH_SOCK set?): {e}"))?;
    #[cfg(windows)]
    let mut agent =
        russh::keys::agent::client::AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent")
            .await
            .map_err(|e| format!("can't reach the OpenSSH agent (`use_agent: true`): {e}"))?;
    let identities = agent
        .request_identities()
        .await
        .map_err(|e| format!("can't list the SSH agent's keys: {e}"))?;
    if identities.is_empty() {
        return Err("the SSH agent holds no keys (`ssh-add` adds one)".into());
    }
    let hash = session
        .best_supported_rsa_hash()
        .await
        .map_err(|e| e.to_string())?
        .flatten();
    for id in identities {
        let key = id.public_key().into_owned();
        let hash = matches!(key.algorithm(), russh::keys::Algorithm::Rsa { .. })
            .then_some(hash)
            .flatten();
        let r = session
            .authenticate_publickey_with(user, key, hash, &mut agent)
            .await
            .map_err(|e| format!("the SSH agent couldn't sign: {e}"))?;
        if r.success() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The SSH settings' problems a static check can find (both a key file and key text, a key
/// that can't be read, no way to sign in), for a plugin's `validate_connection`. No network.
pub fn check_settings(c: &Map<String, Value>, prefix: &str) -> Vec<String> {
    match Ssh::from_settings(c, prefix) {
        Ok(_) => Vec::new(),
        Err(e) => vec![e.to_string()],
    }
}

fn home_known_hosts() -> PathBuf {
    std::env::home_dir()
        .unwrap_or_default()
        .join(".ssh")
        .join("known_hosts")
}

/// Checks the server's host key during the handshake.
pub struct HostCheck {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    fingerprint: Option<String>,
    accept_unknown: bool,
    /// Why the key was refused, for the error message.
    refused: Arc<Mutex<Option<String>>>,
}

impl HostCheck {
    fn verdict(&self, key: &PublicKey) -> std::result::Result<(), String> {
        let fp = key.fingerprint(HashAlg::Sha256).to_string();
        if let Some(pin) = &self.fingerprint {
            let want = if pin.starts_with("SHA256:") {
                pin.clone()
            } else {
                format!("SHA256:{pin}")
            };
            return if want == fp {
                Ok(())
            } else {
                Err(format!(
                    "the server's host key is {fp}, not the pinned `host_key_fingerprint` {want}"
                ))
            };
        }
        match russh::keys::check_known_hosts_path(&self.host, self.port, key, &self.known_hosts) {
            Ok(true) => Ok(()),
            Ok(false) if self.accept_unknown => Ok(()),
            Ok(false) => Err(format!(
                "the host key of {}:{} ({fp}) isn't in {}; add it (ssh-keyscan) or pin `host_key_fingerprint: {fp}`",
                self.host,
                self.port,
                self.known_hosts.display()
            )),
            Err(e) => Err(format!(
                "the host key of {}:{} doesn't match {} ({e}); refusing to connect — it may have changed or be spoofed",
                self.host,
                self.port,
                self.known_hosts.display()
            )),
        }
    }
}

impl client::Handler for HostCheck {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &russh::keys::PublicKeyOrCertificate,
    ) -> std::result::Result<bool, Self::Error> {
        let russh::keys::PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            *self.refused.lock().unwrap() =
                Some("the server presented a certificate; DRE only checks plain host keys".into());
            return Ok(false);
        };
        match self.verdict(key) {
            Ok(()) => Ok(true),
            Err(e) => {
                *self.refused.lock().unwrap() = Some(e);
                Ok(false)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    /// A fresh ed25519 key pair from `ssh-keygen`: (private key file, public key line).
    fn keypair(dir: &Path, passphrase: &str) -> (PathBuf, String) {
        let path = dir.join("id");
        let ok = std::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", passphrase, "-f"])
            .arg(&path)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        // Without the comment, as a server presents its key.
        let public = std::fs::read_to_string(path.with_extension("pub")).unwrap();
        let public = public.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
        (path, public)
    }

    fn settings(v: Value) -> Map<String, Value> {
        let Value::Object(mut m) = json!({"host": "bastion", "username": "dre"}) else {
            unreachable!()
        };
        m.extend(v.as_object().unwrap().clone());
        m
    }

    fn err(c: Map<String, Value>, prefix: &str) -> String {
        match Ssh::from_settings(&c, prefix) {
            Ok(_) => panic!("expected an error"),
            Err(e) => e.to_string(),
        }
    }

    fn check(fingerprint: Option<&str>, known_hosts: &Path) -> HostCheck {
        HostCheck {
            host: "bastion".into(),
            port: 2222,
            known_hosts: known_hosts.into(),
            fingerprint: fingerprint.map(str::to_string),
            accept_unknown: false,
            refused: Arc::default(),
        }
    }

    #[test]
    fn a_leading_tilde_is_the_home_directory() {
        let home = std::env::home_dir().unwrap();
        assert_eq!(expand_home("~/.ssh/id"), home.join(".ssh/id"));
        assert_eq!(expand_home("/etc/key"), PathBuf::from("/etc/key"));
        assert_eq!(expand_home("a~/b"), PathBuf::from("a~/b"));
    }

    #[test]
    fn single_line_key_text_gets_its_line_breaks_back() {
        assert_eq!(
            key_text("-----BEGIN X-----\\nabc\\n-----END X-----\\n"),
            "-----BEGIN X-----\nabc\n-----END X-----\n"
        );
        assert_eq!(key_text("a\\r\\nb"), "a\nb");
        // Real line breaks: left alone (a `\n` inside would be part of the key).
        assert_eq!(key_text("a\nb\\n"), "a\nb\\n");
    }

    #[test]
    fn a_key_is_read_from_a_file_or_from_text() {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = keypair(dir.path(), "");
        let text = std::fs::read_to_string(&path).unwrap();
        for c in [
            json!({"private_key_path": path}),
            json!({"private_key": text}),
            json!({"private_key": text.trim_end().replace('\n', "\\n")}),
        ] {
            let ssh = Ssh::from_settings(&settings(c), "").unwrap();
            assert!(matches!(ssh.auth, Auth::Key(_)));
            assert_eq!(ssh.port, 22);
        }
    }

    #[test]
    fn an_encrypted_key_text_takes_the_passphrase() {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = keypair(dir.path(), "s3cret");
        let text = std::fs::read_to_string(&path).unwrap();
        let ssh = Ssh::from_settings(
            &settings(json!({"private_key": text, "private_key_passphrase": "s3cret"})),
            "",
        );
        assert!(ssh.is_ok());
        let e = err(settings(json!({"private_key": text})), "ssh.");
        assert!(e.contains("can't read `ssh.private_key`"), "{e}");
    }

    #[test]
    fn settings_errors_name_the_field_where_it_lives() {
        let e = err(
            settings(json!({"private_key_path": "a", "private_key": "b"})),
            "ssh.",
        );
        assert_eq!(e, "set `ssh.private_key_path` or `ssh.private_key`, not both");
        let e = err(settings(json!({})), "");
        assert_eq!(
            e,
            "set `password`, `private_key_path`, `private_key` or `use_agent: true`"
        );
        let e = err(json!({"host": "h"}).as_object().unwrap().clone(), "ssh.");
        assert_eq!(e, "the profile output needs a `ssh.username` field");
        let e = err(settings(json!({"password": "x", "port": 70000})), "ssh.");
        assert_eq!(e, "invalid `ssh.port` `70000`");
        let e = err(settings(json!({"private_key": "not a key"})), "");
        assert!(e.starts_with("can't read `private_key`"), "{e}");
    }

    #[test]
    fn host_keys_are_checked_against_a_pin_or_known_hosts() {
        let dir = tempfile::tempdir().unwrap();
        let (_, public) = keypair(dir.path(), "");
        let key = PublicKey::from_openssh(&public).unwrap();
        let fp = key.fingerprint(HashAlg::Sha256).to_string();
        let none = dir.path().join("none");
        std::fs::write(&none, "").unwrap();

        assert!(check(Some(&fp), &none).verdict(&key).is_ok());
        // The pin works without its `SHA256:` prefix too.
        assert!(
            check(Some(fp.trim_start_matches("SHA256:")), &none)
                .verdict(&key)
                .is_ok()
        );
        let e = check(Some("SHA256:AAAAnotthekey"), &none)
            .verdict(&key)
            .unwrap_err();
        assert!(e.contains("not the pinned"), "{e}");

        let e = check(None, &none).verdict(&key).unwrap_err();
        assert!(
            e.contains("isn't in") && e.contains(&format!("pin `host_key_fingerprint: {fp}`")),
            "{e}"
        );

        let kh = dir.path().join("known_hosts");
        std::fs::write(&kh, format!("[bastion]:2222 {public}\n")).unwrap();
        check(None, &kh).verdict(&key).unwrap();

        // Another key for the same host in known_hosts: refused even with accept_unknown.
        let other = tempfile::tempdir().unwrap();
        let (_, public2) = keypair(other.path(), "");
        let key2 = PublicKey::from_openssh(&public2).unwrap();
        let mut lax = check(None, &kh);
        lax.accept_unknown = true;
        let e = lax.verdict(&key2).unwrap_err();
        assert!(e.contains("doesn't match"), "{e}");
    }
}
