//! Offline prediction of RTMR1 for a boot disk.
//!
//! Google's TDX firmware extends RTMR1 with: the "Calling EFI Application"
//! action, a separator, the GPT, the Authenticode SHA-384 of
//! \EFI\BOOT\BOOTX64.EFI and the two ExitBootServices actions. When the
//! kernel's first ExitBootServices call fails (stale memory map) and is
//! retried, a "Returned with Failure" action comes first.

use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha384};
use std::path::Path;
use vcontainer_core::hex;

const CALLING: &str = "Calling EFI Application from Boot Option";
const EBS_INVOCATION: &str = "Exit Boot Services Invocation";
const EBS_SUCCESS: &str = "Exit Boot Services Returned with Success";
const EBS_FAILURE: &str = "Exit Boot Services Returned with Failure";

pub struct Measurement {
    pub pe_sha384: [u8; 48],
    pub gpt_sha384: [u8; 48],
    pub rtmr1: Vec<[u8; 48]>,
}

pub fn measure(efi: &Path, disk: &Path) -> Result<Measurement> {
    let pe_sha384 = authenticode_sha384(&std::fs::read(efi).with_context(|| format!("read {}", efi.display()))?)?;
    let gpt_sha384 = Sha384::digest(gpt_event(&std::fs::read(disk)?)?).into();
    let action = |s: &str| -> [u8; 48] { Sha384::digest(s.as_bytes()).into() };
    let separator: [u8; 48] = Sha384::digest([0u8; 4]).into();
    let head = [action(CALLING), separator, gpt_sha384, pe_sha384];
    let normal = replay(head.iter().chain(&[action(EBS_INVOCATION), action(EBS_SUCCESS)]));
    let retry = replay(head.iter().chain(&[action(EBS_FAILURE), action(EBS_INVOCATION), action(EBS_SUCCESS)]));
    Ok(Measurement { pe_sha384, gpt_sha384, rtmr1: vec![normal, retry] })
}

fn replay<'a>(digests: impl Iterator<Item = &'a [u8; 48]>) -> [u8; 48] {
    digests.fold([0u8; 48], |acc, d| {
        let mut h = Sha384::new();
        h.update(acc);
        h.update(d);
        h.finalize().into()
    })
}

pub fn authenticode_sha384(pe: &[u8]) -> Result<[u8; 48]> {
    let file = object::read::pe::PeFile64::parse(pe).context("BOOTX64.EFI is not a PE32+ image")?;
    let mut h = Sha384::new();
    authenticode::authenticode_digest(&file, &mut h).map_err(|_| anyhow::anyhow!("authenticode digest failed"))?;
    Ok(h.finalize().into())
}

/// UEFI_GPT_DATA as the firmware logs it: the 92-byte primary header, the
/// number of used entries (u64) and those entries.
fn gpt_event(disk: &[u8]) -> Result<Vec<u8>> {
    ensure!(disk.len() >= 1024 && &disk[512..520] == b"EFI PART", "no GPT on the disk image");
    let hdr = &disk[512..512 + 92];
    let u32_at = |o: usize| u32::from_le_bytes(hdr[o..o + 4].try_into().unwrap()) as usize;
    let entries_lba = u64::from_le_bytes(hdr[72..80].try_into().unwrap()) as usize;
    let (count, size) = (u32_at(80), u32_at(84));
    let table = disk.get(entries_lba * 512..entries_lba * 512 + count * size).context("truncated GPT")?;
    let used: Vec<&[u8]> = table.chunks(size).filter(|e| e[..16].iter().any(|&b| b != 0)).collect();
    let mut out = hdr.to_vec();
    out.extend_from_slice(&(used.len() as u64).to_le_bytes());
    for e in used {
        out.extend_from_slice(e);
    }
    Ok(out)
}

pub fn print(m: &Measurement) {
    println!("PE Authenticode SHA-384: {}", hex(&m.pe_sha384));
    println!("GPT event SHA-384:       {}", hex(&m.gpt_sha384));
    println!("expected RTMR1:          {}", hex(&m.rtmr1[0]));
    println!("  (with EBS retry:       {})", hex(&m.rtmr1[1]));
}
