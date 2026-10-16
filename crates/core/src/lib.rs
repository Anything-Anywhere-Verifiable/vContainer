//! The vcontainer protocol, shared by the guest (`vcontainer-init`) and the CLI.
//!
//! A session is one confidential VM. Its `/init` keeps an append-only ledger
//! in TDX RTMR3; every event is `SHA-384(type ‖ ":" ‖ name ‖ ":" ‖ payload)`
//! (the dstack v1 encoding). The ledger of a session is always a prefix of
//! `[boot, owner, launch, exec]`.

pub mod registry;

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha384, Sha512};
use std::collections::BTreeMap;

/// TCP port of the guest's attestation and control API.
pub const PORT: u16 = 7700;
/// Instance metadata key holding the owner's Ed25519 public key (base64).
pub const OWNER_METADATA_KEY: &str = "vcontainer-owner";

pub const EVENT_TYPE: u32 = 0x0800_0001;
pub const EV_BOOT: &str = "vcontainer.boot";
pub const EV_OWNER: &str = "vcontainer.owner";
pub const EV_LAUNCH: &str = "vcontainer.launch";
pub const EV_EXEC: &str = "vcontainer.exec";

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Waiting,
    Starting,
    Pulling,
    Running,
    Exited,
    Failed,
}

impl State {
    /// The ledger length a session in this state must have.
    pub fn ledger_len_ok(self, len: usize) -> bool {
        match self {
            State::Waiting => len == 2,
            State::Starting => len == 3,
            // The exec event is appended just before the helper starts it.
            State::Pulling => len == 3 || len == 4,
            State::Running | State::Exited => len == 4,
            State::Failed => len == 3 || len == 4,
        }
    }
}

/// What the guest reports about its container. Bound into every quote.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Status {
    pub state: State,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
}

impl Status {
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("status serializes")
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Event {
    pub name: String,
    #[serde(with = "b64")]
    pub payload: Vec<u8>,
}

impl Event {
    pub fn new(name: &str, payload: Vec<u8>) -> Self {
        Event { name: name.to_string(), payload }
    }

    pub fn digest(&self) -> [u8; 48] {
        let mut h = Sha384::new();
        h.update(EVENT_TYPE.to_le_bytes());
        h.update(b":");
        h.update(self.name.as_bytes());
        h.update(b":");
        h.update(&self.payload);
        h.finalize().into()
    }
}

/// RTMR3 after extending `events` into an all-zero register.
pub fn replay(events: &[Event]) -> [u8; 48] {
    events.iter().fold([0u8; 48], |acc, e| {
        let mut h = Sha384::new();
        h.update(acc);
        h.update(e.digest());
        h.finalize().into()
    })
}

/// Payload of the boot event: version, boot id and the per-boot HPKE public key.
#[derive(Clone, Debug, PartialEq)]
pub struct BootInfo {
    pub boot_id: [u8; 16],
    pub hpke_pk: [u8; 32],
}

impl BootInfo {
    pub fn encode(&self) -> Vec<u8> {
        let mut v = vec![1u8];
        v.extend_from_slice(&self.boot_id);
        v.extend_from_slice(&self.hpke_pk);
        v
    }

    pub fn decode(b: &[u8]) -> Result<Self> {
        ensure!(b.len() == 49 && b[0] == 1, "bad boot event payload");
        Ok(BootInfo {
            boot_id: b[1..17].try_into().unwrap(),
            hpke_pk: b[17..49].try_into().unwrap(),
        })
    }
}

/// `GET /v1/evidence` response.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Evidence {
    pub v: u32,
    pub tee: String,
    /// Canonical JSON of [`Status`]; its hash is in the quote's REPORTDATA.
    #[serde(with = "b64")]
    pub status: Vec<u8>,
    pub ledger: Vec<Event>,
    #[serde(with = "b64")]
    pub quote: Vec<u8>,
    /// The firmware's CC event log (CCEL), trailing 0xFF padding removed.
    #[serde(with = "b64")]
    pub ccel: Vec<u8>,
}

/// REPORTDATA of a quote answering `nonce`, binding the reported status.
pub fn report_data(nonce: &[u8; 32], status: &[u8]) -> [u8; 64] {
    let mut h = Sha512::new();
    h.update(b"vcontainer/v1/evidence\0");
    h.update(nonce);
    h.update(Sha384::digest(status));
    h.finalize().into()
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Logs {
    Serial,
    None,
}

fn default_logs() -> Logs {
    Logs::Serial
}

/// The launch configuration. Measured into the ledger exactly as sent.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Image reference pinned by digest: `registry/repo@sha256:...`.
    pub image: String,
    #[serde(default)]
    pub entrypoint: Option<Vec<String>>,
    #[serde(default)]
    pub cmd: Option<Vec<String>>,
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub workdir: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    /// Names of sealed secrets, written to `/run/secrets/<name>`.
    #[serde(default)]
    pub secrets: Vec<String>,
    /// Whether the sealed secrets carry a registry token for `image`.
    #[serde(default)]
    pub registry_token: bool,
    #[serde(default = "default_logs")]
    pub logs: Logs,
}

impl Config {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let c: Config = serde_json::from_slice(bytes).context("invalid launch config")?;
        ensure!(c.image.contains("@sha256:"), "image must be pinned by digest");
        for s in &c.secrets {
            ensure!(valid_name(s), "invalid secret name {s:?}");
        }
        Ok(c)
    }
}

pub fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.chars().all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c))
        && !s.starts_with('.')
}

/// Plaintext of the sealed part of a launch.
#[derive(Serialize, Deserialize, Default)]
pub struct Secrets {
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    #[serde(default)]
    pub registry_token: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Sealed {
    #[serde(with = "b64")]
    pub enc: Vec<u8>,
    #[serde(with = "b64")]
    pub ct: Vec<u8>,
}

/// `POST /v1/launch` body.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct LaunchRequest {
    pub boot_id: String,
    pub issued_at: u64,
    #[serde(with = "b64")]
    pub config: Vec<u8>,
    pub sealed: Option<Sealed>,
    #[serde(with = "b64")]
    pub sig: Vec<u8>,
}

/// `POST /v1/commit` body.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CommitRequest {
    pub boot_id: String,
    #[serde(with = "b64")]
    pub sig: Vec<u8>,
}

fn sealed_hash(sealed: Option<&Sealed>) -> [u8; 64] {
    let mut h = Sha512::new();
    if let Some(s) = sealed {
        h.update(&s.enc);
        h.update(&s.ct);
    }
    h.finalize().into()
}

/// The message the owner signs to launch `config` on this boot.
pub fn launch_message(boot: &BootInfo, issued_at: u64, config: &[u8], sealed: Option<&Sealed>) -> [u8; 64] {
    let mut h = Sha512::new();
    h.update(b"vcontainer/v1/launch\0");
    h.update(boot.boot_id);
    h.update(boot.hpke_pk);
    h.update(issued_at.to_le_bytes());
    h.update(Sha512::digest(config));
    h.update(sealed_hash(sealed));
    h.finalize().into()
}

/// Payload of the launch event: the config, a hash of the sealed secrets and
/// the signed time, so the event identifies exactly one signed launch.
pub fn launch_payload(config: &[u8], sealed: Option<&Sealed>, issued_at: u64) -> Vec<u8> {
    let mut v = config.to_vec();
    v.extend_from_slice(&sealed_hash(sealed));
    v.extend_from_slice(&issued_at.to_le_bytes());
    v
}

/// The config bytes inside a launch event payload.
pub fn launch_config(payload: &[u8]) -> Result<&[u8]> {
    ensure!(payload.len() > 72, "launch event payload too short");
    Ok(&payload[..payload.len() - 72])
}

/// The message the owner signs to let exactly this measured launch pull.
pub fn commit_message(boot_id: &[u8; 16], launch_payload: &[u8]) -> [u8; 64] {
    let mut h = Sha512::new();
    h.update(b"vcontainer/v1/commit\0");
    h.update(boot_id);
    h.update(Sha512::digest(launch_payload));
    h.finalize().into()
}

pub mod crypto {
    //! Owner signatures (Ed25519) and sealed secrets (HPKE, RFC 9180 base mode,
    //! X25519-HKDF-SHA256 / HKDF-SHA256 / ChaCha20-Poly1305).
    use super::*;
    use ed25519_dalek::{Signature, Signer};
    pub use ed25519_dalek::{SigningKey, VerifyingKey};
    use hpke::aead::ChaCha20Poly1305;
    use hpke::kdf::HkdfSha256;
    use hpke::kem::X25519HkdfSha256;
    use hpke::{Deserializable, Kem, OpModeR, OpModeS, Serializable};
    use rand_core::OsRng;
    use rand_core_09::{OsRng as OsRng09, UnwrapErr};

    pub type HpkeSecret = <X25519HkdfSha256 as Kem>::PrivateKey;

    pub fn hpke_info(boot_id: &[u8; 16]) -> Vec<u8> {
        let mut v = b"vcontainer/v1/secrets\0".to_vec();
        v.extend_from_slice(boot_id);
        v
    }

    pub fn hpke_keypair() -> (HpkeSecret, [u8; 32]) {
        let (sk, pk) = X25519HkdfSha256::gen_keypair(&mut UnwrapErr(OsRng09));
        (sk, pk.to_bytes().into())
    }

    pub fn seal(boot: &BootInfo, plaintext: &[u8]) -> Result<Sealed> {
        let pk = <X25519HkdfSha256 as Kem>::PublicKey::from_bytes(&boot.hpke_pk)
            .map_err(|e| anyhow::anyhow!("bad HPKE public key: {e:?}"))?;
        let (enc, ct) = hpke::single_shot_seal::<ChaCha20Poly1305, HkdfSha256, X25519HkdfSha256, _>(
            &OpModeS::Base,
            &pk,
            &hpke_info(&boot.boot_id),
            plaintext,
            b"",
            &mut UnwrapErr(OsRng09),
        )
        .map_err(|e| anyhow::anyhow!("HPKE seal failed: {e:?}"))?;
        Ok(Sealed { enc: enc.to_bytes().to_vec(), ct })
    }

    pub fn open(sk: &HpkeSecret, boot_id: &[u8; 16], sealed: &Sealed) -> Result<Vec<u8>> {
        let enc = <X25519HkdfSha256 as Kem>::EncappedKey::from_bytes(&sealed.enc)
            .map_err(|e| anyhow::anyhow!("bad HPKE encapsulated key: {e:?}"))?;
        hpke::single_shot_open::<ChaCha20Poly1305, HkdfSha256, X25519HkdfSha256>(
            &OpModeR::Base,
            sk,
            &enc,
            &hpke_info(boot_id),
            &sealed.ct,
            b"",
        )
        .map_err(|e| anyhow::anyhow!("HPKE open failed: {e:?}"))
    }

    pub fn owner_key_from_seed(seed: &[u8; 32]) -> SigningKey {
        SigningKey::from_bytes(seed)
    }

    pub fn new_owner_seed() -> [u8; 32] {
        SigningKey::generate(&mut OsRng).to_bytes()
    }

    pub fn random32() -> [u8; 32] {
        use rand_core::RngCore;
        let mut b = [0u8; 32];
        OsRng.fill_bytes(&mut b);
        b
    }

    pub fn sign(key: &SigningKey, msg: &[u8; 64]) -> Vec<u8> {
        key.sign(msg).to_bytes().to_vec()
    }

    /// Parses the owner public key as published in instance metadata (base64).
    pub fn parse_owner(metadata: &[u8]) -> Result<VerifyingKey> {
        let raw = super::b64::decode(std::str::from_utf8(metadata)?.trim())?;
        let raw: [u8; 32] = raw.try_into().map_err(|_| anyhow::anyhow!("owner key is not 32 bytes"))?;
        Ok(VerifyingKey::from_bytes(&raw)?)
    }

    pub fn verify(owner: &VerifyingKey, msg: &[u8; 64], sig: &[u8]) -> Result<()> {
        let sig = Signature::from_slice(sig)?;
        owner.verify_strict(msg, &sig).context("bad owner signature")?;
        Ok(())
    }

    pub fn owner_metadata(key: &SigningKey) -> String {
        super::b64::encode(key.verifying_key().as_bytes())
    }
}

pub mod b64 {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn encode(b: &[u8]) -> String {
        STANDARD.encode(b)
    }

    pub fn decode(s: &str) -> anyhow::Result<Vec<u8>> {
        Ok(STANDARD.decode(s)?)
    }

    pub fn serialize<S: Serializer>(b: &Vec<u8>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&encode(b))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        decode(&s).map_err(serde::de::Error::custom)
    }
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn unhex(s: &str) -> Result<Vec<u8>> {
    let digit = |c: u8| (c as char).to_digit(16).map(|d| d as u8).context("invalid hex");
    let b = s.as_bytes();
    ensure!(b.len() % 2 == 0, "odd-length hex");
    b.chunks(2).map(|p| Ok(digit(p[0])? << 4 | digit(p[1])?)).collect()
}

pub fn unhex_array<const N: usize>(s: &str) -> Result<[u8; N]> {
    let v = unhex(s)?;
    match v.try_into() {
        Ok(a) => Ok(a),
        Err(v) => bail!("expected {N} bytes, got {}", v.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledger_replay_and_boot_roundtrip() {
        let boot = BootInfo { boot_id: [7; 16], hpke_pk: [9; 32] };
        assert_eq!(BootInfo::decode(&boot.encode()).unwrap(), boot);
        let e = [Event::new(EV_BOOT, boot.encode())];
        let mut h = Sha384::new();
        h.update([0u8; 48]);
        h.update(e[0].digest());
        let want: [u8; 48] = h.finalize().into();
        assert_eq!(replay(&e), want);
    }

    #[test]
    fn seal_open_sign_verify() {
        let (sk, pk) = crypto::hpke_keypair();
        let boot = BootInfo { boot_id: [1; 16], hpke_pk: pk };
        let sealed = crypto::seal(&boot, b"secret").unwrap();
        assert_eq!(crypto::open(&sk, &boot.boot_id, &sealed).unwrap(), b"secret");
        let key = crypto::owner_key_from_seed(&[3; 32]);
        let owner = crypto::parse_owner(crypto::owner_metadata(&key).as_bytes()).unwrap();
        let msg = launch_message(&boot, 5, b"{}", Some(&sealed));
        crypto::verify(&owner, &msg, &crypto::sign(&key, &msg)).unwrap();
        assert!(crypto::verify(&owner, &launch_message(&boot, 6, b"{}", Some(&sealed)), &crypto::sign(&key, &msg)).is_err());
    }

    #[test]
    fn config_requires_digest() {
        assert!(Config::parse(br#"{"image":"docker.io/library/alpine:3"}"#).is_err());
        assert!(Config::parse(br#"{"image":"docker.io/library/alpine@sha256:00","bogus":1}"#).is_err());
        let c = Config::parse(br#"{"image":"docker.io/library/alpine@sha256:00"}"#).unwrap();
        assert_eq!(c.logs, Logs::Serial);
    }
}
