//! A minimal OCI distribution client: manifests and blobs, verified by digest.
//!
//! Credentials (`oauth2accesstoken` + an access token for Artifact Registry,
//! or any user/password) are only ever sent over HTTPS to the registry host
//! itself, never to a token realm on another host and never on redirects.

use anyhow::{anyhow, bail, ensure, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::time::Duration;

pub const MANIFEST_ACCEPT: &str = "application/vnd.oci.image.index.v1+json, \
application/vnd.docker.distribution.manifest.list.v2+json, \
application/vnd.oci.image.manifest.v1+json, \
application/vnd.docker.distribution.manifest.v2+json";

const MAX_MANIFEST: u64 = 4 << 20;

#[derive(Debug, Clone, PartialEq)]
pub struct ImageRef {
    /// Registry host as written by the user (`docker.io` for Docker Hub).
    pub registry: String,
    pub repo: String,
    pub tag: Option<String>,
    pub digest: Option<String>,
}

impl ImageRef {
    pub fn parse(s: &str) -> Result<Self> {
        let (rest, digest) = match s.split_once('@') {
            Some((r, d)) => {
                ensure!(d.starts_with("sha256:") && d.len() == 71, "unsupported digest {d}");
                (r, Some(d.to_string()))
            }
            None => (s, None),
        };
        let (registry, path) = match rest.split_once('/') {
            Some((first, path)) if first.contains('.') || first.contains(':') || first == "localhost" => {
                (first.to_string(), path.to_string())
            }
            _ => ("docker.io".to_string(), rest.to_string()),
        };
        // A tag is the part after the last ':' of the last path component.
        let (repo, tag) = match path.rsplit_once(':') {
            Some((r, t)) if !t.contains('/') => (r.to_string(), Some(t.to_string())),
            _ => (path, None),
        };
        let repo = if registry == "docker.io" && !repo.contains('/') { format!("library/{repo}") } else { repo };
        ensure!(!repo.is_empty(), "empty repository in {s}");
        let tag = if tag.is_none() && digest.is_none() { Some("latest".to_string()) } else { tag };
        Ok(ImageRef { registry, repo, tag, digest })
    }

    /// Host serving the distribution API.
    pub fn api_host(&self) -> &str {
        if self.registry == "docker.io" { "registry-1.docker.io" } else { &self.registry }
    }

    /// `registry/repo@digest`, the form measured into the ledger.
    pub fn pinned(&self, digest: &str) -> String {
        format!("{}/{}@{}", self.registry, self.repo, digest)
    }

    /// Whether this is a Google Artifact Registry / Container Registry host.
    pub fn is_google(&self) -> bool {
        let h = self.registry.as_str();
        h == "gcr.io" || h.ends_with(".gcr.io") || (h.ends_with("-docker.pkg.dev") && !h.contains(['/', '@', ':']))
    }
}

pub fn sha256_digest(b: &[u8]) -> String {
    format!("sha256:{}", crate::hex(&Sha256::digest(b)))
}

#[derive(Deserialize)]
pub struct Descriptor {
    #[serde(rename = "mediaType", default)]
    pub media_type: String,
    pub digest: String,
    pub size: u64,
    #[serde(default)]
    pub platform: Option<Platform>,
}

#[derive(Deserialize)]
pub struct Platform {
    pub architecture: String,
    pub os: String,
    #[serde(default)]
    pub variant: Option<String>,
}

#[derive(Deserialize)]
pub struct Manifest {
    #[serde(rename = "mediaType", default)]
    pub media_type: String,
    #[serde(default)]
    pub manifests: Vec<Descriptor>,
    pub config: Option<Descriptor>,
    #[serde(default)]
    pub layers: Vec<Descriptor>,
}

impl Manifest {
    pub fn is_index(&self) -> bool {
        !self.manifests.is_empty() || self.media_type.contains("index") || self.media_type.contains("list")
    }

    pub fn linux_amd64(&self) -> Result<&Descriptor> {
        self.manifests
            .iter()
            .find(|d| d.platform.as_ref().is_some_and(|p| p.os == "linux" && p.architecture == "amd64"))
            .ok_or_else(|| anyhow!("image index has no linux/amd64 manifest"))
    }
}

/// The parts of an image config that decide how the container starts.
#[derive(Deserialize, Default)]
pub struct ImageConfig {
    #[serde(default)]
    pub config: RunConfig,
}

#[derive(Deserialize, Default)]
pub struct RunConfig {
    #[serde(rename = "Entrypoint", default)]
    pub entrypoint: Option<Vec<String>>,
    #[serde(rename = "Cmd", default)]
    pub cmd: Option<Vec<String>>,
    #[serde(rename = "Env", default)]
    pub env: Option<Vec<String>>,
    #[serde(rename = "WorkingDir", default)]
    pub workdir: Option<String>,
    #[serde(rename = "User", default)]
    pub user: Option<String>,
}

pub struct Registry {
    agent: ureq::Agent,
    image: ImageRef,
    /// (user, password) for the registry host only.
    creds: Option<(String, String)>,
    /// Value of the Authorization header once authenticated.
    auth: Option<String>,
}

impl Registry {
    pub fn new(image: &ImageRef, creds: Option<(String, String)>) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_recv_body(Some(Duration::from_secs(600)))
            .http_status_as_error(false)
            .build()
            .into();
        Registry { agent, image: image.clone(), creds, auth: None }
    }

    fn url(&self, kind: &str, reference: &str) -> String {
        format!("https://{}/v2/{}/{}/{}", self.image.api_host(), self.image.repo, kind, reference)
    }

    fn get(&mut self, url: &str, accept: &str) -> Result<ureq::http::Response<ureq::Body>> {
        for attempt in 0..2 {
            let mut req = self.agent.get(url).header("Accept", accept);
            if let Some(a) = &self.auth {
                req = req.header("Authorization", a);
            }
            let resp = req.call().with_context(|| format!("GET {url}"))?;
            if resp.status() == 401 && attempt == 0 {
                let challenge = resp
                    .headers()
                    .get("www-authenticate")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                self.authenticate(&challenge)?;
                continue;
            }
            ensure!(resp.status().is_success(), "GET {url}: HTTP {}", resp.status());
            return Ok(resp);
        }
        bail!("GET {url}: unauthorized")
    }

    /// Answers a `WWW-Authenticate` challenge (RFC 6750 / Docker token auth).
    fn authenticate(&mut self, challenge: &str) -> Result<()> {
        let (scheme, params) = challenge.split_once(' ').unwrap_or((challenge, ""));
        let param = |k: &str| -> Option<String> {
            params.split(',').find_map(|kv| {
                let (key, v) = kv.trim().split_once('=')?;
                (key == k).then(|| v.trim_matches('"').to_string())
            })
        };
        let basic = self.creds.as_ref().map(|(u, p)| format!("Basic {}", crate::b64::encode(format!("{u}:{p}").as_bytes())));
        if scheme.eq_ignore_ascii_case("basic") {
            self.auth = Some(basic.context("registry requires credentials")?);
            return Ok(());
        }
        ensure!(scheme.eq_ignore_ascii_case("bearer"), "unsupported auth challenge {challenge:?}");
        let realm = param("realm").context("auth challenge without realm")?;
        ensure!(realm.starts_with("https://"), "token realm must be https: {realm}");
        let realm_host = realm["https://".len()..].split(['/', '?']).next().unwrap_or("");
        let service = param("service").unwrap_or_default();
        let scope = param("scope").unwrap_or_else(|| format!("repository:{}:pull", self.image.repo));
        let url = format!("{realm}?service={}&scope={}", urlencode(&service), urlencode(&scope));
        let mut req = self.agent.get(&url);
        // Credentials go only to a realm on the registry host itself.
        if let Some(b) = basic.filter(|_| realm_host == self.image.api_host()) {
            req = req.header("Authorization", b);
        }
        let mut resp = req.call().with_context(|| format!("GET {realm}"))?;
        ensure!(resp.status().is_success(), "token request to {realm}: HTTP {}", resp.status());
        #[derive(Deserialize)]
        struct Token {
            token: Option<String>,
            access_token: Option<String>,
        }
        let t: Token = serde_json::from_slice(&resp.body_mut().with_config().limit(1 << 20).read_to_vec()?)?;
        let token = t.token.or(t.access_token).context("token response without token")?;
        self.auth = Some(format!("Bearer {token}"));
        Ok(())
    }

    /// Fetches a manifest by tag or digest. Returns (bytes, digest of bytes).
    /// A digest reference is checked against the bytes received.
    pub fn manifest(&mut self, reference: &str) -> Result<(Vec<u8>, String)> {
        let url = self.url("manifests", reference);
        let mut resp = self.get(&url, MANIFEST_ACCEPT)?;
        let body = resp.body_mut().with_config().limit(MAX_MANIFEST).read_to_vec()?;
        let digest = sha256_digest(&body);
        // A tag has no ':'; anything else is a digest and must be ours.
        if reference.contains(':') {
            ensure!(digest == reference, "manifest digest mismatch: wanted {reference}, got {digest}");
        }
        Ok((body, digest))
    }

    /// Streams a blob into `out`, failing unless it has exactly `size` bytes
    /// with digest `digest`.
    pub fn blob(&mut self, digest: &str, size: u64, out: &mut dyn Write) -> Result<()> {
        ensure!(digest.starts_with("sha256:"), "unsupported blob digest {digest}");
        let url = self.url("blobs", digest);
        let resp = self.get(&url, "*/*")?;
        let mut reader = resp.into_body().into_reader().take(size + 1);
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 16];
        let mut total = 0u64;
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            ensure!(total <= size, "blob {digest} is larger than its descriptor ({size} bytes)");
            hasher.update(&buf[..n]);
            out.write_all(&buf[..n])?;
        }
        ensure!(total == size, "blob {digest}: got {total} bytes, expected {size}");
        let got = format!("sha256:{}", crate::hex(&hasher.finalize()));
        ensure!(got == digest, "blob digest mismatch: wanted {digest}, got {got}");
        Ok(())
    }

    /// Resolves the reference to (pinned index-or-manifest digest).
    pub fn resolve(&mut self) -> Result<String> {
        let reference = self.image.digest.clone().or(self.image.tag.clone()).unwrap();
        Ok(self.manifest(&reference)?.1)
    }
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_refs() {
        let r = ImageRef::parse("python:3.13-slim").unwrap();
        assert_eq!((r.registry.as_str(), r.repo.as_str(), r.tag.as_deref()), ("docker.io", "library/python", Some("3.13-slim")));
        assert_eq!(r.api_host(), "registry-1.docker.io");
        let d = format!("sha256:{}", "a".repeat(64));
        let r = ImageRef::parse(&format!("us-central1-docker.pkg.dev/p/r/agent@{d}")).unwrap();
        assert_eq!(r.registry, "us-central1-docker.pkg.dev");
        assert_eq!(r.repo, "p/r/agent");
        assert_eq!(r.digest.as_deref(), Some(d.as_str()));
        assert!(r.is_google());
        assert!(!ImageRef::parse("evilgcr.io/x/y:1").unwrap().is_google());
        assert!(ImageRef::parse("us.gcr.io/x/y:1").unwrap().is_google());
        let r = ImageRef::parse("localhost:5000/x/y").unwrap();
        assert_eq!((r.registry.as_str(), r.repo.as_str(), r.tag.as_deref()), ("localhost:5000", "x/y", Some("latest")));
    }
}

#[cfg(test)]
mod live {
    //! Network tests: `cargo test -p vcontainer-core -- --ignored`.
    use super::*;

    fn pull_config(image: &str, creds: Option<(String, String)>) -> ImageConfig {
        let r = ImageRef::parse(image).unwrap();
        let mut reg = Registry::new(&r, creds);
        let digest = reg.resolve().unwrap();
        let mut m: Manifest = serde_json::from_slice(&reg.manifest(&digest).unwrap().0).unwrap();
        if m.is_index() {
            let d = m.linux_amd64().unwrap().digest.clone();
            m = serde_json::from_slice(&reg.manifest(&d).unwrap().0).unwrap();
        }
        let c = m.config.unwrap();
        let mut raw = Vec::new();
        reg.blob(&c.digest, c.size, &mut raw).unwrap();
        let l = &m.layers[0];
        reg.blob(&l.digest, l.size, &mut std::io::sink()).unwrap();
        assert!(reg.blob(&l.digest, l.size - 1, &mut std::io::sink()).is_err());
        serde_json::from_slice(&raw).unwrap()
    }

    #[test]
    #[ignore]
    fn docker_hub_anonymous() {
        let c = pull_config("alpine:3.20", None);
        assert!(c.config.cmd.is_some());
    }

    #[test]
    #[ignore]
    fn artifact_registry_token() {
        let Ok(token) = std::env::var("AR_TOKEN") else { return };
        let image = std::env::var("AR_IMAGE").unwrap();
        let c = pull_config(&image, Some(("oauth2accesstoken".into(), token)));
        assert!(c.config.cmd.is_some());
    }
}
