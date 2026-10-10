//! DRE destination plugin `ftp`: plain FTP or explicit FTPS (`AUTH TLS`).
//!
//! Profile target fields: `host`, `port` (21), `username`, `password`, `passive` (default true),
//! `tls`: `none` (default) or `explicit`, and `tls_accept_invalid_certs` (default false, for
//! servers with self-signed certificates). Missing remote directories are created.
//!
//! FTPS data connections resume the control connection's TLS session, as most servers require
//! (vsftpd's `require_ssl_reuse`). A failed upload removes the partial remote file when it can.

use std::net::ToSocketAddrs;
use std::path::Path;

use dre_protocol::msg::ConnectionField;
use dre_protocol::plugin::{
    About, Delivery, Destination, PluginError, Result, conn_bool, conn_required, conn_str, serve_destination,
};
use serde_json::{Map, Value};
use std::sync::Arc;

use dre_protocol::delivery::{self, Caps, Rules, Store, StoreError, deliver};
use dre_protocol::options::OptionField;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use rustls_platform_verifier::BuilderVerifierExt;
use suppaftp::{Mode, RustlsConnector, RustlsFtpStream};

struct Ftp;

/// Connects to the first of `host`'s addresses that answers, like `TcpStream::connect` does:
/// `localhost` resolves to both `::1` and `127.0.0.1`, and a server may listen on only one.
/// `rules` gives the connect timeout and the control connection's no-progress timeout.
fn connect(host: &str, port: u16, rules: &Rules) -> std::result::Result<RustlsFtpStream, String> {
    let mut last = None;
    for addr in (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("can't resolve {host}: {e}"))?
    {
        match RustlsFtpStream::connect_timeout(addr, rules.connect_timeout) {
            Ok(ftp) => {
                let _ = ftp.get_ref().set_read_timeout(Some(rules.timeout));
                let _ = ftp.get_ref().set_write_timeout(Some(rules.timeout));
                return Ok(ftp);
            }
            Err(e) => last = Some(e),
        }
    }
    Err(match last {
        Some(e) => format!("can't connect to {host}:{port}: {e}"),
        None => format!("can't resolve {host}"),
    })
}

/// The TLS setup for control and data connections. One config is shared by both, so its
/// session cache lets the data connections resume the control connection's session.
///
/// TLS 1.2 first: under TLS 1.3 the server sends session tickets on every data connection,
/// which nothing reads, so closing it resets the connection and vsftpd fails the upload.
/// `tls13` is the fallback for servers that only speak TLS 1.3.
fn tls_config(accept_invalid: bool, tls13: bool) -> std::result::Result<Arc<ClientConfig>, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let versions: &[&rustls::SupportedProtocolVersion] = if tls13 {
        &[&rustls::version::TLS13]
    } else {
        &[&rustls::version::TLS12]
    };
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(versions)
        .map_err(|e| e.to_string())?;
    let config = if accept_invalid {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAny(provider)))
            .with_no_client_auth()
    } else {
        builder
            .with_platform_verifier()
            .map_err(|e| format!("can't load the system's certificates: {e}"))?
            .with_no_client_auth()
    };
    Ok(Arc::new(config))
}

/// `tls_accept_invalid_certs: true`: any certificate and host name, signatures still checked.
#[derive(Debug)]
struct AcceptAny(Arc<rustls::crypto::CryptoProvider>);

impl ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn upload(
    local: &Path,
    remote: &str,
    c: &Map<String, Value>,
    options: &Map<String, Value>,
) -> Result<String> {
    let host = conn_required(c, "host")?;
    let port: u16 = match c.get("port") {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(21) as u16,
        Some(Value::String(s)) => s.parse().map_err(|_| format!("invalid `port` `{s}`"))?,
        _ => 21,
    };
    let user = conn_required(c, "username")?;
    let password = conn_str(c, "password").unwrap_or("");
    let (rules, _) = Rules::from_settings(Rules::default(), c, options, &[])?;
    let mut ftp = connect(host, port, &rules)?;
    match conn_str(c, "tls").unwrap_or("none") {
        "none" => {}
        "explicit" => {
            let accept_invalid = conn_bool(c, "tls_accept_invalid_certs").unwrap_or(false);
            ftp = match ftp.into_secure(RustlsConnector::from(tls_config(accept_invalid, false)?), host) {
                Ok(f) => f,
                Err(e12) => {
                    // Maybe a TLS 1.3-only server: once more, on a new connection.
                    let again = connect(host, port, &rules)?;
                    again
                        .into_secure(RustlsConnector::from(tls_config(accept_invalid, true)?), host)
                        .map_err(|_| format!("{host}:{port} didn't accept explicit FTPS (AUTH TLS): {e12}"))?
                }
            };
        }
        t => return Err(format!("unknown `tls` `{t}` (none or explicit)").into()),
    }
    ftp.login(user, password)
        .map_err(|e| format!("{host}:{port} refused the credentials for `{user}`: {e}"))?;
    ftp.set_mode(if conn_bool(c, "passive").unwrap_or(true) {
        Mode::Passive
    } else {
        Mode::Active
    });
    ftp.transfer_type(suppaftp::types::FileType::Binary)?;
    let mut store = FtpStore { ftp };
    let delivered = deliver(&mut store, local, remote, &rules).map_err(PluginError::from)?;
    let _ = store.ftp.quit();
    Ok(format!(
        "ftp://{user}@{host}:{port}/{}",
        delivered.path.trim_start_matches('/')
    ))
}

/// One FTP control connection, as the shared delivery rules' store. FTP can't create a file only
/// if it's missing, nor rename without replacing, so the rules look first.
struct FtpStore {
    ftp: RustlsFtpStream,
}

fn failed(what: &str, path: &str, e: impl std::fmt::Display) -> StoreError {
    StoreError::Failed(format!("can't {what} {path}: {e}"))
}

impl FtpStore {
    /// Create `path`'s missing parent folders (each prefix in turn; ones that exist fail
    /// harmlessly).
    fn make_parents(&mut self, path: &str) {
        let Some((dir, _)) = path.rsplit_once('/') else {
            return;
        };
        let mut prefix = String::new();
        if dir.starts_with('/') {
            prefix.push('/');
        }
        for part in dir.split('/').filter(|p| !p.is_empty()) {
            if !prefix.is_empty() && !prefix.ends_with('/') {
                prefix.push('/');
            }
            prefix.push_str(part);
            let _ = self.ftp.mkdir(&prefix);
        }
    }
}

impl Store for FtpStore {
    fn caps(&self) -> Caps {
        Caps::default()
    }

    fn write(&mut self, local: &Path, remote: &str, _exclusive: bool) -> std::result::Result<(), StoreError> {
        self.make_parents(remote);
        let mut f = std::fs::File::open(local)
            .map_err(|e| StoreError::Failed(format!("can't read {}: {e}", local.display())))?;
        self.ftp
            .put_file(remote, &mut f)
            .map(|_| ())
            .map_err(|e| failed("upload to", remote, e))
    }

    fn rename(&mut self, from: &str, to: &str, replace: bool) -> std::result::Result<(), StoreError> {
        self.make_parents(to);
        match self.ftp.rename(from, to) {
            Ok(()) => Ok(()),
            // A server whose rename won't replace a file: remove it, then rename.
            Err(_) if replace && self.ftp.size(to).is_ok() => {
                self.ftp.rm(to).map_err(|e| failed("replace", to, e))?;
                self.ftp.rename(from, to).map_err(|e| failed("rename to", to, e))
            }
            Err(e) => Err(failed("rename to", to, e)),
        }
    }

    fn exists(&mut self, remote: &str) -> std::result::Result<bool, StoreError> {
        Ok(self.ftp.size(remote).is_ok())
    }

    fn delete(&mut self, remote: &str) -> std::result::Result<(), StoreError> {
        let _ = self.ftp.rm(remote);
        Ok(())
    }
}

impl Destination for Ftp {
    fn connection_fields(&self) -> Vec<ConnectionField> {
        vec![
            ConnectionField::new("host", "FTP server").required(),
            ConnectionField::new("port", "port").default(21),
            ConnectionField::new("username", "user name").required(),
            ConnectionField::new("password", "password").secret(),
            ConnectionField::new("tls", "none or explicit (FTPS)").default("none"),
        ]
        .into_iter()
        .chain(dre_protocol::delivery::timeout_fields())
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
            return Err("ftp takes one file per delivery".into());
        };
        let remote = f.remote.as_deref().ok_or("ftp needs `output.destination.path`")?;
        upload(&f.local, remote, &d.connection, &d.options)
    }
}

fn main() {
    serve_destination(About::new("ftp", env!("CARGO_PKG_VERSION")), Ftp)
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::net::TcpListener;

    #[test]
    fn connect_falls_back_to_the_next_resolved_address() {
        // Listen on IPv4 only; `localhost` often resolves to `::1` first.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            s.write_all(b"220 ready\r\n").unwrap();
        });
        super::connect("localhost", port, &dre_protocol::delivery::Rules::default()).unwrap();
        server.join().unwrap();
    }
}
