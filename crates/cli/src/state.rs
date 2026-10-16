//! Local state in ~/.config/vcontainer: the owner key, the published release
//! and one file per session. A session file embeds the session's verification
//! policy, which is also what a third party needs to verify it.

use anyhow::{bail, ensure, Context, Result};
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
    /// The clean source commit the image was built from (build-info.json).
    pub commit: Option<String>,
}

/// The GCE instance a quote must come from (TDX MROWNER is SHA-384 of these).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Instance {
    pub id: u64,
    pub project_number: u64,
    pub zone: String,
}

/// Everything a verifier needs. Shareable: contains no secrets. Unknown fields
/// are rejected, so a misspelled field cannot silently drop its check.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub tee: String,
    /// Owner public key, base64, exactly as in instance metadata.
    pub owner: String,
    pub release: Release,
    pub instance: Instance,
    /// The launch event payload (base64 of the exact bytes) once deployed.
    pub launch: Option<String>,
    pub min_tcb_eval_num: u32,
    /// Lowest acceptable security version of Google's TDX firmware.
    pub min_fw_svn: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Session {
    pub name: String,
    pub project: String,
    pub zone: String,
    pub ip: String,
    /// Address range the session's firewall rule admits.
    pub source: String,
    /// Container ports of the launch, opened to `source`.
    pub ports: Vec<u16>,
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
        std::io::Write::write_all(&mut f, &crypto::random::<32>())?;
        eprintln!("created owner key {}", path.display());
    }
    let meta = fs::metadata(&path)?;
    if meta.permissions().mode() & 0o077 != 0 {
        bail!("{} must not be readable by others (chmod 600)", path.display());
    }
    let seed: [u8; 32] = fs::read(&path)?.try_into().map_err(|_| anyhow::anyhow!("{} is not a 32-byte key", path.display()))?;
    Ok(SigningKey::from_bytes(&seed))
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

/// Session names are GCE instance names and part of firewall rule names.
fn valid_session_name(name: &str) -> bool {
    name.len() <= 40 && name.starts_with(|c: char| c.is_ascii_lowercase()) && !name.ends_with('-') && name.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

fn session_path(name: &str) -> Result<PathBuf> {
    ensure!(valid_session_name(name), "session names are up to 40 lowercase letters, digits and '-', starting with a letter and not ending with '-' (got {name:?})");
    Ok(dir()?.join("sessions").join(format!("{name}.json")))
}

/// The session `name`, or None if it has no file. A file that cannot be read
/// or parsed is an error: its VM may still exist.
pub fn find_session(name: &str) -> Result<Option<Session>> {
    let path = session_path(name)?;
    match fs::read(&path) {
        Ok(raw) => Ok(Some(serde_json::from_slice(&raw).with_context(|| format!("parse {}", path.display()))?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

pub fn load_session(name: &str) -> Result<Session> {
    find_session(name)?.with_context(|| format!("no session {name:?}; start it with `vcontainer run --session {name}`"))
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
            match serde_json::from_slice(&fs::read(&p)?) {
                Ok(s) => out.push(s),
                Err(e) => eprintln!("warning: unreadable session file {}: {e}", p.display()),
            }
        }
    }
    out.sort_by(|a: &Session, b| a.name.cmp(&b.name));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_names() {
        let longest = "a".repeat(40);
        for ok in ["a", "web-1", "a1-b2", longest.as_str()] {
            assert!(valid_session_name(ok), "{ok}");
        }
        let too_long = "a".repeat(41);
        for bad in ["", "1a", "-a", "a-", "Ab", "a_b", "a.b", "a/b", "a b", too_long.as_str()] {
            assert!(!valid_session_name(bad), "{bad}");
        }
    }

    #[test]
    fn policy_fields() {
        let release = Release { image: "i".into(), project: "p".into(), pe_authenticode_sha384: "00".into(), gpt_event_sha384: "00".into(), commit: None };
        let instance = Instance { id: 1, project_number: 2, zone: "z".into() };
        let p = Policy { tee: "tdx".into(), owner: "o".into(), release, instance, launch: None, min_tcb_eval_num: 20, min_fw_svn: 2 };
        let v = serde_json::to_value(&p).unwrap();
        assert!(serde_json::from_value::<Policy>(v.clone()).unwrap().launch.is_none());
        let mut without_launch = v.clone();
        without_launch.as_object_mut().unwrap().remove("launch");
        assert!(serde_json::from_value::<Policy>(without_launch).unwrap().launch.is_none());
        let mut unknown = v.clone();
        unknown["config"] = "e30=".into();
        assert!(serde_json::from_value::<Policy>(unknown).is_err());
        for required in ["instance", "min_fw_svn"] {
            let mut missing = v.clone();
            missing.as_object_mut().unwrap().remove(required);
            assert!(serde_json::from_value::<Policy>(missing).is_err(), "{required}");
        }
    }
}
