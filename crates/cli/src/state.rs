//! Local state in ~/.config/vcontainer: the owner key, the published release
//! and one file per session. A session file embeds the session's verification
//! policy, which is also what a third party needs to verify it.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use vcontainer_core::crypto::{self, SigningKey};

/// Expected measurements of a published boot image (from `vcontainer measure`).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Release {
    /// GCE image name and the project it lives in.
    pub image: String,
    pub project: String,
    pub pe_authenticode_sha384: String,
    pub gpt_event_sha384: String,
    /// Source commit and build time the image was built from (build-info.json).
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub source_date_epoch: Option<u64>,
}

/// The GCE instance a quote must come from (TDX MROWNER is SHA-384 of these).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Instance {
    pub id: u64,
    pub project_number: u64,
    pub zone: String,
}

/// Everything a verifier needs. Shareable: contains no secrets.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Policy {
    pub tee: String,
    /// Owner public key, base64, exactly as in instance metadata.
    pub owner: String,
    pub release: Release,
    #[serde(default)]
    pub instance: Option<Instance>,
    /// The launch config (base64 of the exact bytes) once deployed.
    #[serde(default)]
    pub config: Option<String>,
    pub min_tcb_eval_num: u32,
    /// Lowest acceptable security version of Google's TDX firmware.
    #[serde(default = "default_fw_svn")]
    pub min_fw_svn: u64,
}

/// Google's TDX firmware security version as of October 2026.
pub fn default_fw_svn() -> u64 {
    2
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Session {
    pub name: String,
    pub project: String,
    pub zone: String,
    pub machine_type: String,
    pub ip: String,
    /// Address range the session's firewall rule admits.
    pub source: String,
    #[serde(default)]
    pub ports: Vec<u16>,
    /// A signed launch sent but not yet committed (base64 of the launch
    /// event payload), so an interrupted deploy can be resumed.
    #[serde(default)]
    pub pending: Option<String>,
    pub policy: Policy,
}

impl Session {
    pub fn url(&self) -> String {
        format!("http://{}:{}", self.ip, vcontainer_core::PORT)
    }
}

pub fn dir() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME is not set")?;
    let d = std::env::var("VCONTAINER_HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(home).join(".config/vcontainer"));
    fs::create_dir_all(d.join("sessions"))?;
    Ok(d)
}

/// The owner key, created on first use (32-byte Ed25519 seed, mode 0600).
pub fn owner_key() -> Result<SigningKey> {
    let path = dir()?.join("owner.key");
    if !path.exists() {
        let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
        std::io::Write::write_all(&mut f, &crypto::new_owner_seed())?;
        eprintln!("created owner key {}", path.display());
    }
    let meta = fs::metadata(&path)?;
    if meta.permissions().mode() & 0o077 != 0 {
        bail!("{} must not be readable by others (chmod 600)", path.display());
    }
    let seed: [u8; 32] = fs::read(&path)?.try_into().map_err(|_| anyhow::anyhow!("{} is not a 32-byte key", path.display()))?;
    Ok(crypto::owner_key_from_seed(&seed))
}

pub fn release() -> Result<Release> {
    let path = dir()?.join("release.json");
    let raw = fs::read(&path).with_context(|| {
        format!("{} not found: build and publish the boot image first (scripts/build-image.sh, then `vcontainer publish`)", path.display())
    })?;
    Ok(serde_json::from_slice(&raw)?)
}

pub fn save_release(r: &Release) -> Result<()> {
    fs::write(dir()?.join("release.json"), serde_json::to_vec_pretty(r)?)?;
    Ok(())
}

fn session_path(name: &str) -> Result<PathBuf> {
    if !vcontainer_core::valid_name(name) || name.len() > 40 || !name.chars().next().unwrap().is_ascii_lowercase() || name.chars().any(|c| c.is_ascii_uppercase() || c == '_' || c == '.') {
        bail!("session names are lowercase letters, digits and '-', starting with a letter (got {name:?})");
    }
    Ok(dir()?.join("sessions").join(format!("{name}.json")))
}

pub fn load_session(name: &str) -> Result<Session> {
    let path = session_path(name)?;
    let raw = fs::read(&path).with_context(|| format!("no session {name:?}; start it with `vcontainer run --session {name}`"))?;
    Ok(serde_json::from_slice(&raw)?)
}

pub fn save_session(s: &Session) -> Result<()> {
    fs::write(session_path(&s.name)?, serde_json::to_vec_pretty(s)?)?;
    Ok(())
}

pub fn remove_session(name: &str) -> Result<()> {
    let _ = fs::remove_file(session_path(name)?);
    Ok(())
}

pub fn sessions() -> Result<Vec<Session>> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir()?.join("sessions"))? {
        let p = e?.path();
        if p.extension().is_some_and(|x| x == "json") {
            if let Ok(s) = serde_json::from_slice(&fs::read(&p)?) {
                out.push(s);
            }
        }
    }
    out.sort_by(|a: &Session, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Session names double as GCE instance names.
pub fn check_name(name: &str) -> Result<()> {
    session_path(name).map(|_| ())
}
