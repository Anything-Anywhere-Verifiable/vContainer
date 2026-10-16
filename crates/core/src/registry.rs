//! A minimal OCI distribution client: manifests and blobs, verified by digest.
//!
//! The only credential is a Google access token, sent over HTTPS only to the
//! registry host's own token realm, never to another host and never on redirects.

use anyhow::{ensure, Context, Result};
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
    /// The digest if there is one, else the tag (`latest` by default).
    pub reference: String,
}

impl ImageRef {
    pub fn parse(s: &str) -> Result<Self> {
        let (rest, digest) = match s.split_once('@') {
            Some((r, d)) => {
                ensure!(valid_digest(d), "unsupported digest {d}");
                (r, Some(d))
            }
            None => (s, None),
        };
        let (registry, path) = match rest.split_once('/') {
            Some((first, path)) if first.contains('.') || first.contains(':') || first == "localhost" => (first, path),
            _ => ("docker.io", rest),
        };
        // A tag is the part after the last ':' of the last path component.
        let (repo, tag) = match path.rsplit_once(':') {
            Some((r, t)) if !t.contains('/') => (r, Some(t)),
            _ => (path, None),
        };
        let repo = if registry == "docker.io" && !repo.contains('/') { format!("library/{repo}") } else { repo.to_string() };
        ensure!(valid_host(registry), "invalid registry host {registry:?}");
        let alnum = |ch: char| ch.is_ascii_alphanumeric();
        ensure!(
            repo.split('/').all(|c| c.starts_with(alnum) && c.ends_with(alnum) && c.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || "._-".contains(ch))),
            "invalid repository {repo:?}"
        );
        if let Some(t) = tag {
            ensure!(!t.is_empty() && t.len() <= 128 && t.chars().all(|ch| ch.is_ascii_alphanumeric() || "._-".contains(ch)), "invalid tag {t:?}");
        }
        Ok(ImageRef { registry: registry.to_string(), repo, reference: digest.or(tag).unwrap_or("latest").to_string() })
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
        h == "gcr.io" || h.ends_with(".gcr.io") || h.ends_with("-docker.pkg.dev")
    }
}

/// `host[:port]` with DNS characters only (no userinfo, path, query or fragment).
fn valid_host(h: &str) -> bool {
    let (host, port) = h.split_once(':').unwrap_or((h, "1"));
    !host.is_empty()
        && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        && !port.is_empty()
        && port.chars().all(|c| c.is_ascii_digit())
}

/// `sha256:` followed by 64 lowercase hex digits.
pub fn valid_digest(d: &str) -> bool {
    d.strip_prefix("sha256:").is_some_and(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)))
}

pub fn sha256_digest(b: &[u8]) -> String {
    format!("sha256:{}", crate::hex(&Sha256::digest(b)))
}

#[derive(Deserialize)]
pub struct Descriptor {
    pub digest: String,
    pub size: u64,
    platform: Option<Platform>,
}

#[derive(Deserialize)]
struct Platform {
    architecture: String,
    os: String,
}

#[derive(Deserialize)]
pub struct Manifest {
    pub config: Descriptor,
    pub layers: Vec<Descriptor>,
}

#[derive(Deserialize)]
struct Index {
    manifests: Vec<Descriptor>,
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
    /// `oauth2accesstoken:<token>` as Basic credentials, for the registry host's own token realm.
    basic: Option<String>,
    /// Value of the Authorization header once authenticated.
    auth: Option<String>,
}

impl Registry {
    /// `token`: a Google access token, for Artifact Registry and Container Registry.
    pub fn new(image: &ImageRef, token: Option<String>) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_recv_response(Some(Duration::from_secs(60)))
            .timeout_recv_body(Some(Duration::from_secs(3600)))
            .http_status_as_error(false)
            .build()
            .into();
        let basic = token.map(|t| format!("Basic {}", crate::b64::encode(format!("oauth2accesstoken:{t}").as_bytes())));
        Registry { agent, image: image.clone(), basic, auth: None }
    }

    fn url(&self, kind: &str, reference: &str) -> String {
        format!("https://{}/v2/{}/{}/{}", self.image.api_host(), self.image.repo, kind, reference)
    }

    fn send(&self, url: &str, accept: &str) -> Result<ureq::http::Response<ureq::Body>> {
        let mut req = self.agent.get(url).header("Accept", accept);
        if let Some(a) = &self.auth {
            req = req.header("Authorization", a);
        }
        req.call().with_context(|| format!("GET {url}"))
    }

    /// Sends; on 401, answers the challenge and sends once more.
    fn get(&mut self, url: &str, accept: &str) -> Result<ureq::http::Response<ureq::Body>> {
        let mut resp = self.send(url, accept)?;
        if resp.status() == 401 {
            let challenge = resp.headers().get("www-authenticate").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
            self.authenticate(&challenge)?;
            resp = self.send(url, accept)?;
        }
        ensure!(resp.status().is_success(), "GET {url}: HTTP {}", resp.status());
        Ok(resp)
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
        ensure!(scheme.eq_ignore_ascii_case("bearer"), "unsupported auth challenge {challenge:?}");
        let realm = param("realm").context("auth challenge without realm")?;
        ensure!(realm.starts_with("https://"), "token realm must be https: {realm}");
        let realm_host = realm["https://".len()..].split(['/', '?']).next().unwrap_or("");
        let service = param("service").unwrap_or_default();
        let scope = param("scope").unwrap_or_else(|| format!("repository:{}:pull", self.image.repo));
        let url = format!("{realm}?service={}&scope={}", urlencode(&service), urlencode(&scope));
        let mut req = self.agent.get(&url);
        // The access token goes only to a realm on the registry host itself.
        if let Some(b) = self.basic.as_ref().filter(|_| realm_host == self.image.api_host()) {
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

    fn fetch_manifest(&mut self, reference: &str) -> Result<Vec<u8>> {
        let url = self.url("manifests", reference);
        let mut resp = self.get(&url, MANIFEST_ACCEPT)?;
        Ok(resp.body_mut().with_config().limit(MAX_MANIFEST).read_to_vec()?)
    }

    /// Fetches the manifest with this digest, checked against the bytes.
    fn manifest(&mut self, digest: &str) -> Result<Vec<u8>> {
        ensure!(valid_digest(digest), "unsupported manifest digest {digest:?}");
        let body = self.fetch_manifest(digest)?;
        let got = sha256_digest(&body);
        ensure!(got == digest, "manifest digest mismatch: wanted {digest}, got {got}");
        Ok(body)
    }

    /// The linux/amd64 image manifest that `digest` names, directly or through
    /// an image index, with that manifest's own digest.
    pub fn image_manifest(&mut self, digest: &str) -> Result<(String, Manifest)> {
        let body = self.manifest(digest)?;
        if let Ok(m) = serde_json::from_slice(&body) {
            return Ok((digest.to_string(), m));
        }
        let index: Index = serde_json::from_slice(&body).context("neither an image manifest nor an image index")?;
        let d = index
            .manifests
            .iter()
            .find(|d| d.platform.as_ref().is_some_and(|p| p.os == "linux" && p.architecture == "amd64"))
            .context("image index has no linux/amd64 manifest")?;
        Ok((d.digest.clone(), serde_json::from_slice(&self.manifest(&d.digest)?)?))
    }

    /// The image config a manifest names.
    pub fn config(&mut self, d: &Descriptor) -> Result<ImageConfig> {
        ensure!(d.size <= MAX_MANIFEST, "image config too large");
        let mut raw = Vec::new();
        self.blob(&d.digest, d.size, &mut raw)?;
        Ok(serde_json::from_slice(&raw)?)
    }

    /// Streams a blob into `out`, failing unless it has exactly `size` bytes
    /// with digest `digest`.
    pub fn blob(&mut self, digest: &str, size: u64, out: &mut dyn Write) -> Result<()> {
        ensure!(valid_digest(digest), "unsupported blob digest {digest:?}");
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

    /// The digest the reference names (a tag is resolved by the registry).
    pub fn resolve(&mut self) -> Result<String> {
        let r = self.image.reference.clone();
        Ok(if valid_digest(&r) { r } else { sha256_digest(&self.fetch_manifest(&r)?) })
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
        assert_eq!((r.registry.as_str(), r.repo.as_str(), r.reference.as_str()), ("docker.io", "library/python", "3.13-slim"));
        assert_eq!(r.api_host(), "registry-1.docker.io");
        let d = format!("sha256:{}", "a".repeat(64));
        let r = ImageRef::parse(&format!("us-central1-docker.pkg.dev/p/r/agent@{d}")).unwrap();
        assert_eq!(r.registry, "us-central1-docker.pkg.dev");
        assert_eq!(r.repo, "p/r/agent");
        assert_eq!(r.reference, d);
        assert!(r.is_google());
        assert!(!ImageRef::parse("evilgcr.io/x/y:1").unwrap().is_google());
        assert!(ImageRef::parse("attacker.example?.gcr.io/p/i:tag").is_err());
        assert!(ImageRef::parse("user@host.gcr.io/p/i:tag").is_err());
        assert!(ImageRef::parse("us.gcr.io/x/y:1").unwrap().is_google());
        let r = ImageRef::parse("localhost:5000/x/y").unwrap();
        assert_eq!((r.registry.as_str(), r.repo.as_str(), r.reference.as_str()), ("localhost:5000", "x/y", "latest"));
        assert!(ImageRef::parse("reg/a/../b").is_err());
    }
}

#[cfg(test)]
mod live {
    //! Network tests: `cargo test -p vcontainer-core -- --ignored`.
    use super::*;

    fn pull_config(image: &str, token: Option<String>) -> ImageConfig {
        let r = ImageRef::parse(image).unwrap();
        let mut reg = Registry::new(&r, token);
        let digest = reg.resolve().unwrap();
        let (_, m) = reg.image_manifest(&digest).unwrap();
        let l = &m.layers[0];
        reg.blob(&l.digest, l.size, &mut std::io::sink()).unwrap();
        assert!(reg.blob(&l.digest, l.size - 1, &mut std::io::sink()).is_err());
        reg.config(&m.config).unwrap()
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
        let c = pull_config(&image, Some(token));
        assert!(c.config.cmd.is_some());
    }
}
