//! Google's signed launch endorsement for the TDX firmware.
//!
//! `gce_tcb_integrity/ovmf_x64_csm/tdx/<MRTD>.binarypb` is a
//! VMLaunchEndorsement { 1: VMGoldenMeasurement bytes, 2: signature }. The
//! golden measurement carries the signer certificate (field 4) and the TDX
//! measurements (field 8 → 2 → 3: MRTD). The signature is RSA-PSS-SHA256
//! (salt 32) by the signer, whose certificate GCE-cc-tcb-root signed the same
//! way. We pin the root, never the signer, which rotates.

use anyhow::{bail, ensure, Context, Result};
use rsa::pkcs8::DecodePublicKey;
use rsa::{Pss, RsaPublicKey};
use sha2::{Digest, Sha256};
use vcontainer_core::hex;
use x509_cert::der::{Decode, Encode};
use x509_cert::Certificate;

/// https://pki.goog/cloud_integrity/GCE-cc-tcb-root_1.crt
/// (SHA-256 e876bc6978bf4f3da445f98a0a82363c8c0bae5a1fc033c6df65846a6cb0f18c)
const ROOT: &[u8] = include_bytes!("GCE-cc-tcb-root_1.crt");

/// Checks that Google endorses `mrtd`; returns the firmware's security version.
pub fn check_mrtd(mrtd: &[u8; 48]) -> Result<u64> {
    let blob = fetch(mrtd)?;
    let top = fields(&blob)?;
    let golden = get(&top, 1).context("endorsement without golden measurement")?;
    let signature = get(&top, 2).context("endorsement without signature")?;
    let g = fields(golden)?;
    let signer = Certificate::from_der(get(&g, 4).context("no signer certificate")?)?;

    let root = Certificate::from_der(ROOT)?;
    let validity = &signer.tbs_certificate.validity;
    let now = std::time::SystemTime::now();
    ensure!(
        validity.not_before.to_system_time() <= now && now <= validity.not_after.to_system_time(),
        "the endorsement signer certificate is not valid now"
    );
    let root_key = public_key(&root)?;
    pss_verify(&root_key, &signer.tbs_certificate.to_der()?, signer.signature.raw_bytes()).context("signer certificate is not signed by GCE-cc-tcb-root")?;
    pss_verify(&public_key(&signer)?, golden, signature).context("endorsement signature")?;

    let tdx = fields(get(&g, 8).context("endorsement has no TDX section")?)?;
    let svn = tdx.iter().find(|f| f.0 == 1).map(|f| f.1).unwrap_or(0);
    for (field, _, bytes) in &tdx {
        if *field == 2 && fields(bytes)?.iter().any(|m| m.0 == 3 && m.2 == mrtd) {
            return Ok(svn);
        }
    }
    bail!("Google's endorsement does not list MRTD {}", hex(mrtd))
}

/// Checks that the quote's platform (PPID from Intel's PCK certificate) is a
/// Google Cloud host, using Google's public host registry.
pub fn check_host(ppid: &[u8]) -> Result<()> {
    let url = format!("https://storage.googleapis.com/confidential-host-registry/{}", hex(ppid));
    crate::verify::http(&url, None).map(drop).with_context(|| format!("PPID {} is not in Google's host registry", hex(ppid)))
}

fn fetch(mrtd: &[u8; 48]) -> Result<Vec<u8>> {
    let cache = crate::state::dir()?.join("endorsements");
    std::fs::create_dir_all(&cache)?;
    let path = cache.join(format!("{}.binarypb", hex(mrtd)));
    if let Ok(b) = std::fs::read(&path) {
        return Ok(b);
    }
    let url = format!("https://storage.googleapis.com/gce_tcb_integrity/ovmf_x64_csm/tdx/{}.binarypb", hex(mrtd));
    let b = crate::verify::http(&url, None).context("no Google endorsement for this MRTD")?;
    std::fs::write(&path, &b)?;
    Ok(b)
}

fn public_key(c: &Certificate) -> Result<RsaPublicKey> {
    Ok(RsaPublicKey::from_public_key_der(&c.tbs_certificate.subject_public_key_info.to_der()?)?)
}

fn pss_verify(key: &RsaPublicKey, msg: &[u8], sig: &[u8]) -> Result<()> {
    key.verify(Pss::new_with_salt::<Sha256>(32), &Sha256::digest(msg), sig)?;
    Ok(())
}

fn get<'a>(f: &'a [(u64, u64, &'a [u8])], n: u64) -> Option<&'a [u8]> {
    f.iter().find(|x| x.0 == n).map(|x| x.2)
}

/// Minimal protobuf reader: (field number, varint value, bytes).
fn fields(mut b: &[u8]) -> Result<Vec<(u64, u64, &[u8])>> {
    fn varint(b: &mut &[u8]) -> Result<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let (&c, rest) = b.split_first().context("truncated protobuf")?;
            *b = rest;
            v |= ((c & 0x7f) as u64) << shift;
            if c & 0x80 == 0 {
                return Ok(v);
            }
        }
        bail!("bad varint")
    }
    let mut out = Vec::new();
    while !b.is_empty() {
        let key = varint(&mut b)?;
        let (field, wire) = (key >> 3, key & 7);
        match wire {
            0 => out.push((field, varint(&mut b)?, &[][..])),
            2 => {
                let len = varint(&mut b)? as usize;
                ensure!(len <= b.len(), "truncated protobuf");
                out.push((field, 0, &b[..len]));
                b = &b[len..];
            }
            1 | 5 => {
                let len = if wire == 1 { 8 } else { 4 };
                ensure!(len <= b.len(), "truncated protobuf");
                b = &b[len..];
            }
            _ => bail!("unsupported protobuf wire type {wire}"),
        }
    }
    Ok(out)
}
