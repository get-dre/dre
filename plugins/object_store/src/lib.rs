//! DRE destination plugins for object storage: `s3`, `gcs` and `azure_blob`.
//!
//! Each binary streams the local file up in parts, so memory stays flat however large the
//! output is: multipart uploads for S3 and Azure, a resumable JSON-API upload for GCS (which also
//! works against the fake-gcs-server emulator). The remote path is `s3://bucket/key`, `gs://bucket/key` or
//! `az://container/key`; a path without a scheme is a key inside the profile's `bucket` /
//! `container`.
//!
//! Profile target fields:
//!
//! - **s3**: `bucket`, `region`, `access_key_id` + `secret_access_key` (+ `session_token`), or
//!   none of them to use AWS's default credential chain: the environment, the shared
//!   config/credentials files (`profile`, else `AWS_PROFILE`), SSO, `credential_process`, web
//!   identity, and container or instance roles (`AWS_EC2_METADATA_DISABLED` is honoured). The
//!   region falls back to the AWS config's. `endpoint` and `allow_http` for S3-compatible stores.
//! - **gcs**: `bucket`, `service_account_key_path` (a key file) or `service_account_key` (the
//!   key JSON), or neither for application default credentials (`GOOGLE_APPLICATION_CREDENTIALS`,
//!   the `gcloud auth application-default login` file, or the metadata server); `endpoint` for
//!   emulators.
//! - **azure_blob**: `account_name`, `container`, and one of `connection_string`, `sas_token`,
//!   `access_key`, `use_managed_identity: true`, or `use_azure_cli: true` (the `az login`
//!   session); `endpoint` for emulators.

use std::path::Path;
use std::sync::Arc;

use dre_protocol::delivery::{self, Caps, Rules, Store, StoreError, deliver, timeout_fields};
use dre_protocol::msg::ConnectionField;
use dre_protocol::options::OptionField;
use dre_protocol::plugin::{
    About, Delivery, Destination, Plugin, PluginError, Result, conn_bool, conn_str, serve_package,
};
use dre_protocol::util::percent_encode;
use object_store::ClientOptions;
use object_store::aws::AmazonS3Builder;
use object_store::azure::{AzureConfigKey, MicrosoftAzureBuilder};
use object_store::buffered::BufWriter;
use object_store::gcp::{GoogleCloudStorageBuilder, GoogleConfigKey};
use object_store::path::Path as ObjectPath;
use object_store::{ObjectStore, ObjectStoreExt, PutMode, PutOptions};
use serde_json::{Map, Value};
use tokio::io::AsyncWriteExt;

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    S3,
    Gcs,
    Azure,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::S3 => "s3",
            Kind::Gcs => "gcs",
            Kind::Azure => "azure_blob",
        }
    }
    fn scheme(self) -> &'static str {
        match self {
            Kind::S3 => "s3",
            Kind::Gcs => "gs",
            Kind::Azure => "az",
        }
    }
}

/// Split `scheme://bucket/key` (or a bare key) into bucket and key.
fn locate(kind: Kind, remote: Option<&str>, c: &Map<String, Value>) -> Result<(String, String)> {
    let default_bucket = match kind {
        Kind::Azure => conn_str(c, "container"),
        _ => conn_str(c, "bucket"),
    };
    let remote = remote.ok_or("this destination needs `output.destination.path`")?;
    for scheme in [kind.scheme(), if kind == Kind::Azure { "azure" } else { "" }] {
        if scheme.is_empty() {
            continue;
        }
        if let Some(rest) = remote.strip_prefix(&format!("{scheme}://")) {
            let (bucket, key) = rest
                .split_once('/')
                .ok_or_else(|| format!("`{remote}` has no object key after the bucket"))?;
            return Ok((bucket.to_string(), key.to_string()));
        }
    }
    if remote.contains("://") {
        return Err(format!("`{remote}` isn't a {}:// path", kind.scheme()).into());
    }
    let bucket = default_bucket.ok_or_else(|| {
        format!(
            "`{remote}` has no {}:// prefix and the profile has no `{}`",
            kind.scheme(),
            if kind == Kind::Azure {
                "container"
            } else {
                "bucket"
            }
        )
    })?;
    Ok((bucket.to_string(), remote.trim_start_matches('/').to_string()))
}

fn build(
    kind: Kind,
    bucket: &str,
    c: &Map<String, Value>,
    rt: &tokio::runtime::Runtime,
) -> Result<Arc<dyn ObjectStore>> {
    // `connect_timeout`, and `timeout` per request (uploads go in parts).
    let (rules, _) = Rules::from_settings(Rules::default(), c, &Map::new(), &[])?;
    let client = ClientOptions::new()
        .with_connect_timeout(rules.connect_timeout)
        .with_timeout(rules.timeout);
    Ok(match kind {
        Kind::S3 => {
            let explicit = conn_str(c, "access_key_id").is_some();
            let mut b = if explicit {
                AmazonS3Builder::new()
            } else {
                AmazonS3Builder::from_env()
            };
            b = b.with_client_options(client).with_bucket_name(bucket);
            if let Some(r) = conn_str(c, "region") {
                b = b.with_region(r);
            }
            // No keys: AWS's default chain, resolved (and checked) now so a missing login
            // fails at once with what was tried.
            if !explicit {
                let (creds, region) = aws_chain(conn_str(c, "profile"), rt)?;
                b = b.with_credentials(creds);
                if conn_str(c, "region").is_none()
                    && let Some(r) = region
                {
                    b = b.with_region(r);
                }
            }
            if explicit {
                b = b
                    .with_access_key_id(conn_str(c, "access_key_id").unwrap())
                    .with_secret_access_key(
                        conn_str(c, "secret_access_key")
                            .ok_or("`access_key_id` needs `secret_access_key`")?,
                    );
                if let Some(t) = conn_str(c, "session_token") {
                    b = b.with_token(t);
                }
            }
            if let Some(e) = conn_str(c, "endpoint") {
                b = b.with_endpoint(e).with_virtual_hosted_style_request(false);
            }
            if conn_bool(c, "allow_http").unwrap_or(false)
                || conn_str(c, "endpoint").is_some_and(|e| e.starts_with("http://"))
            {
                b = b.with_allow_http(true);
            }
            Arc::new(b.build()?)
        }
        Kind::Gcs => {
            let mut b = GoogleCloudStorageBuilder::from_env()
                .with_client_options(client)
                .with_bucket_name(bucket);
            if let Some(p) = conn_str(c, "service_account_key_path") {
                b = b.with_service_account_path(p);
            } else if let Some(k) = conn_str(c, "service_account_key") {
                b = b.with_service_account_key(k);
            }
            if let Some(e) = conn_str(c, "endpoint") {
                b = b.with_config(GoogleConfigKey::BaseUrl, e);
            }
            Arc::new(b.build()?)
        }
        Kind::Azure => {
            let mut b = MicrosoftAzureBuilder::new()
                .with_client_options(client.clone())
                .with_container_name(bucket);
            let mut account = conn_str(c, "account_name").map(str::to_string);
            let mut endpoint = conn_str(c, "endpoint").map(str::to_string);
            if let Some(cs) = conn_str(c, "connection_string") {
                let parts: std::collections::HashMap<String, String> = cs
                    .split(';')
                    .filter_map(|kv| {
                        kv.split_once('=')
                            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
                    })
                    .collect();
                if let Some(a) = parts.get("accountname") {
                    account = Some(a.clone());
                }
                if let Some(k) = parts.get("accountkey") {
                    b = b.with_access_key(k);
                }
                if let Some(sas) = parts.get("sharedaccesssignature") {
                    b = b.with_config(AzureConfigKey::SasKey, sas);
                }
                if let Some(e) = parts.get("blobendpoint") {
                    endpoint = Some(e.clone());
                }
            } else if let Some(sas) = conn_str(c, "sas_token") {
                b = b.with_config(AzureConfigKey::SasKey, sas.trim_start_matches('?'));
            } else if let Some(k) = conn_str(c, "access_key") {
                b = b.with_access_key(k);
            } else if conn_bool(c, "use_managed_identity").unwrap_or(false) {
                b = MicrosoftAzureBuilder::from_env()
                    .with_client_options(client.clone())
                    .with_container_name(bucket);
            } else if conn_bool(c, "use_azure_cli").unwrap_or(false) {
                b = b.with_config(AzureConfigKey::UseAzureCli, "true");
            } else {
                return Err("azure_blob needs `connection_string`, `sas_token`, `access_key`, `use_managed_identity: true` or `use_azure_cli: true`".into());
            }
            let account =
                account.ok_or("azure_blob needs `account_name` (or a connection string with AccountName)")?;
            b = b.with_account(account);
            if let Some(e) = endpoint {
                if e.starts_with("http://") {
                    b = b.with_allow_http(true);
                }
                b = b.with_endpoint(e);
            }
            Arc::new(b.build()?)
        }
    })
}

/// AWS's default credential chain as an object_store credential provider, plus its region.
fn aws_chain(
    profile: Option<&str>,
    rt: &tokio::runtime::Runtime,
) -> Result<(object_store::aws::AwsCredentialProvider, Option<String>)> {
    use aws_credential_types::provider::ProvideCredentials;
    let (provider, region) = rt.block_on(async {
        let mut loader = aws_config::defaults(aws_config::BehaviorVersion::latest());
        if let Some(p) = profile {
            loader = loader.profile_name(p);
        }
        let cfg = loader.load().await;
        let provider = cfg
            .credentials_provider()
            .ok_or("the AWS SDK has no credential provider")?;
        provider.provide_credentials().await.map_err(|e| {
            let profile = profile
                .map(|p| format!("profile `{p}`"))
                .or_else(|| std::env::var("AWS_PROFILE").ok().map(|p| format!("AWS_PROFILE `{p}`")))
                .unwrap_or_else(|| "the default profile".into());
            format!(
                "no AWS credentials found (tried: environment variables, the shared config and credentials files with {profile}, SSO, credential_process, web identity, container and instance roles): {}",
                aws_error(&e)
            )
        })?;
        Ok::<_, String>((provider, cfg.region().map(|r| r.to_string())))
    })?;
    Ok((Arc::new(AwsChain { provider }), region))
}

/// An error and its causes, on one line.
fn aws_error(e: &dyn std::error::Error) -> String {
    let mut msg = e.to_string();
    let mut src = e.source();
    while let Some(s) = src {
        let t = s.to_string();
        if !msg.contains(&t) {
            msg = format!("{msg}: {t}");
        }
        src = s.source();
    }
    msg
}

#[derive(Debug)]
struct AwsChain {
    provider: aws_credential_types::provider::SharedCredentialsProvider,
}

#[async_trait::async_trait]
impl object_store::CredentialProvider for AwsChain {
    type Credential = object_store::aws::AwsCredential;

    async fn get_credential(&self) -> object_store::Result<Arc<Self::Credential>> {
        use aws_credential_types::provider::ProvideCredentials;
        // The SDK's chain caches and refreshes before expiry.
        let c = self
            .provider
            .provide_credentials()
            .await
            .map_err(|e| object_store::Error::Generic {
                store: "S3",
                source: aws_error(&e).into(),
            })?;
        Ok(Arc::new(object_store::aws::AwsCredential {
            key_id: c.access_key_id().to_string(),
            secret_key: c.secret_access_key().to_string(),
            token: c.session_token().map(str::to_string),
        }))
    }
}

pub struct ObjectStoreDestination {
    kind: Kind,
}

impl Destination for ObjectStoreDestination {
    fn connection_fields(&self) -> Vec<ConnectionField> {
        let mut fields = match self.kind {
            Kind::S3 => vec![
                ConnectionField::new("bucket", "default bucket (or use s3://bucket/... paths)"),
                ConnectionField::new("region", "AWS region, e.g. ap-southeast-2"),
                ConnectionField::new(
                    "access_key_id",
                    "access key id (leave empty to use the ambient credential chain)",
                )
                .secret(),
                ConnectionField::new("secret_access_key", "secret access key").secret(),
            ],
            Kind::Gcs => vec![
                ConnectionField::new("bucket", "default bucket (or use gs://bucket/... paths)"),
                ConnectionField::new(
                    "service_account_key_path",
                    "service-account key file (empty: application default credentials)",
                ),
            ],
            Kind::Azure => vec![
                ConnectionField::new("account_name", "storage account name").required(),
                ConnectionField::new("container", "default container (or use az://container/... paths)"),
                ConnectionField::new(
                    "connection_string",
                    "connection string (or set sas_token / access_key)",
                )
                .secret(),
            ],
        };
        fields.extend(timeout_fields());
        fields
    }

    /// `if_exists` (the shared delivery rules). An object only appears once its upload is
    /// complete, so there's no temporary name.
    fn options(&self) -> Vec<OptionField> {
        delivery::option_fields()
            .into_iter()
            .filter(|f| f.name == "if_exists")
            .collect()
    }

    fn deliver_files(&mut self, d: &Delivery) -> Result<String> {
        let [f] = d.files.as_slice() else {
            return Err("this destination takes one file per delivery".into());
        };
        let (bucket, key) = locate(self.kind, f.remote.as_deref(), &d.connection)?;
        let (rules, _) = Rules::from_settings(Rules::default(), &d.connection, &d.options, &[])?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let store = build(self.kind, &bucket, &d.connection, &rt)?;
        let mut s = ObjStore {
            kind: self.kind,
            bucket: bucket.clone(),
            connection: &d.connection,
            rt: &rt,
            store,
        };
        let delivered = deliver(&mut s, &f.local, &key, &rules).map_err(PluginError::from)?;
        Ok(format!("{}://{bucket}/{}", self.kind.scheme(), delivered.path))
    }
}

/// An object already has the name (a conditional upload refused).
#[derive(Debug)]
struct Exists;

impl std::fmt::Display for Exists {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("an object is already there")
    }
}

impl std::error::Error for Exists {}

/// One bucket or container, as the shared delivery rules' store.
struct ObjStore<'a> {
    kind: Kind,
    bucket: String,
    connection: &'a Map<String, Value>,
    rt: &'a tokio::runtime::Runtime,
    store: Arc<dyn ObjectStore>,
}

impl Store for ObjStore<'_> {
    fn caps(&self) -> Caps {
        Caps {
            create_exclusive: true,
            rename_no_replace: false,
            visible_when_complete: true,
        }
    }

    fn write(&mut self, local: &Path, key: &str, exclusive: bool) -> std::result::Result<(), StoreError> {
        let location = format!("{}://{}/{key}", self.kind.scheme(), self.bucket);
        if self.kind == Kind::Gcs {
            return match gcs_upload(&self.bucket, key, local, self.connection, exclusive) {
                Ok(_) => Ok(()),
                Err(e) if e.is::<Exists>() => Err(StoreError::Exists),
                Err(e) => Err(StoreError::Failed(e.to_string())),
            };
        }
        let path = ObjectPath::from(key);
        self.rt.block_on(async {
            if exclusive {
                // A conditional upload: refused when an object has the name.
                let bytes = tokio::fs::read(local)
                    .await
                    .map_err(|e| StoreError::Failed(format!("can't read {}: {e}", local.display())))?;
                let opts = PutOptions {
                    mode: PutMode::Create,
                    ..Default::default()
                };
                return match self.store.put_opts(&path, bytes.into(), opts).await {
                    Ok(_) => Ok(()),
                    Err(object_store::Error::AlreadyExists { .. })
                    | Err(object_store::Error::Precondition { .. }) => Err(StoreError::Exists),
                    Err(e) => Err(StoreError::Failed(format!("upload to {location} failed: {e}"))),
                };
            }
            let mut file = tokio::fs::File::open(local)
                .await
                .map_err(|e| StoreError::Failed(format!("can't read {}: {e}", local.display())))?;
            let mut w = BufWriter::with_capacity(self.store.clone(), path, 8 * 1024 * 1024);
            if let Err(e) = tokio::io::copy(&mut file, &mut w).await {
                let _ = w.abort().await;
                return Err(StoreError::Failed(format!("upload to {location} failed: {e}")));
            }
            w.shutdown()
                .await
                .map_err(|e| StoreError::Failed(format!("upload to {location} failed: {e}")))
        })
    }

    fn rename(&mut self, _from: &str, to: &str, _replace: bool) -> std::result::Result<(), StoreError> {
        Err(StoreError::Failed(format!(
            "can't rename to {to}: object stores need no temporary name"
        )))
    }

    fn exists(&mut self, key: &str) -> std::result::Result<bool, StoreError> {
        match self.rt.block_on(self.store.head(&ObjectPath::from(key))) {
            Ok(_) => Ok(true),
            Err(object_store::Error::NotFound { .. }) => Ok(false),
            Err(e) => Err(StoreError::Failed(format!("can't look for {key}: {e}"))),
        }
    }

    fn delete(&mut self, key: &str) -> std::result::Result<(), StoreError> {
        let _ = self.rt.block_on(self.store.delete(&ObjectPath::from(key)));
        Ok(())
    }
}

const GCS_CHUNK: usize = 8 * 1024 * 1024;

/// Upload to GCS with the JSON API's resumable protocol, `GCS_CHUNK` bytes at a time.
/// Credentials (service-account key, application default credentials) come from object_store.
fn gcs_upload(
    bucket: &str,
    key: &str,
    local: &Path,
    c: &Map<String, Value>,
    exclusive: bool,
) -> Result<String> {
    let location = format!("gs://{bucket}/{key}");
    let base = conn_str(c, "endpoint")
        .map(str::to_string)
        .or_else(|| {
            let p = conn_str(c, "service_account_key_path")?;
            let v: Value = serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()?;
            v.get("gcs_base_url")?.as_str().map(str::to_string)
        })
        .unwrap_or_else(|| "https://storage.googleapis.com".to_string());
    let base = base.trim_end_matches('/');
    let mut b = GoogleCloudStorageBuilder::from_env().with_bucket_name(bucket);
    if let Some(p) = conn_str(c, "service_account_key_path") {
        b = b.with_service_account_path(p);
    } else if let Some(k) = conn_str(c, "service_account_key") {
        b = b.with_service_account_key(k);
    }
    let store = b.build()?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let token = rt
        .block_on(async { store.credentials().get_credential().await })
        .map_err(|e| format!("GCS credentials: {e}"))?;
    let auth = (!token.bearer.is_empty()).then(|| format!("Bearer {}", token.bearer));
    let size = std::fs::metadata(local)
        .map_err(|e| format!("can't read {}: {e}", local.display()))?
        .len();
    let fail = |what: &str| format!("upload to {location} failed: {what}");
    // 308 means "resume incomplete" in this protocol, not a redirect.
    let (rules, _) = Rules::from_settings(Rules::default(), c, &Map::new(), &[])?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_connect(Some(rules.connect_timeout))
        .timeout_recv_response(Some(rules.timeout))
        .build()
        .into();

    // `ifGenerationMatch=0`: only if no object has the name (`if_exists: error | number`).
    let start = format!(
        "{base}/upload/storage/v1/b/{}/o?uploadType=resumable&name={}{}",
        percent_encode(bucket, false),
        percent_encode(key, false),
        if exclusive { "&ifGenerationMatch=0" } else { "" }
    );
    let mut req = agent
        .post(&start)
        .header("X-Upload-Content-Length", &size.to_string())
        .header("Content-Type", "application/json");
    if let Some(a) = &auth {
        req = req.header("Authorization", a);
    }
    let mut resp = req.send("{}").map_err(|e| fail(&e.to_string()))?;
    if resp.status().as_u16() == 412 {
        return Err(Box::new(Exists));
    }
    if !resp.status().is_success() {
        let body = resp.body_mut().read_to_string().unwrap_or_default();
        return Err(fail(&format!(
            "HTTP {}: {}",
            resp.status(),
            body.chars().take(300).collect::<String>()
        ))
        .into());
    }
    let session = resp
        .headers()
        .get("Location")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| fail("the server didn't start an upload session"))?
        .to_string();

    use std::io::Read;
    let mut file = std::fs::File::open(local)?;
    let mut buf = vec![0u8; GCS_CHUNK];
    let mut offset = 0u64;
    loop {
        let mut n = 0;
        while n < buf.len() {
            let r = file.read(&mut buf[n..])?;
            if r == 0 {
                break;
            }
            n += r;
        }
        let last = offset + n as u64 >= size;
        let range = if n == 0 {
            format!("bytes */{size}")
        } else {
            format!("bytes {offset}-{}/{size}", offset + n as u64 - 1)
        };
        let mut put = agent.put(&session).header("Content-Range", &range);
        if let Some(a) = &auth {
            put = put.header("Authorization", a);
        }
        let mut resp = put.send(&buf[..n]).map_err(|e| fail(&e.to_string()))?;
        let status = resp.status().as_u16();
        match status {
            200 | 201 => break,
            412 => return Err(Box::new(Exists)),
            308 if !last => {}
            _ => {
                let body = resp.body_mut().read_to_string().unwrap_or_default();
                return Err(fail(&format!(
                    "HTTP {status}: {}",
                    body.chars().take(300).collect::<String>()
                ))
                .into());
            }
        }
        offset += n as u64;
    }
    Ok(location)
}

/// Serve the package: the `s3`, `gcs` and `azure_blob` destinations.
pub fn serve() -> ! {
    serve_package(
        [Kind::S3, Kind::Gcs, Kind::Azure]
            .into_iter()
            .map(|kind| {
                Plugin::Destination(
                    About::new(kind.name(), env!("CARGO_PKG_VERSION")),
                    Box::new(ObjectStoreDestination { kind }),
                )
            })
            .collect(),
    )
}
