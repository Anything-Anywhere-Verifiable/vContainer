//! Intel TDX guest interfaces (upstream Linux ABIs, no extra drivers):
//! RTMR3 extension through sysfs (Linux 6.16+), quotes through configfs-tsm
//! (6.7+) and the firmware's CC event log through ACPI sysfs (6.4+).

use anyhow::{ensure, Context, Result};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

const RTMR3: &str = "/sys/devices/virtual/misc/tdx_guest/measurements/rtmr3:sha384";
const TSM: &str = "/sys/kernel/config/tsm/report";
const CCEL: &str = "/sys/firmware/acpi/tables/data/CCEL";

pub fn check() -> Result<()> {
    ensure!(Path::new(RTMR3).exists(), "not a TDX guest: {RTMR3} is missing");
    ensure!(Path::new(TSM).exists(), "configfs-tsm is missing ({TSM})");
    Ok(())
}

/// RTMR3 = SHA-384(RTMR3 ‖ digest). The kernel requires one 48-byte write.
pub fn extend_rtmr3(digest: &[u8; 48]) -> Result<()> {
    let mut f = fs::OpenOptions::new().write(true).open(RTMR3).context("open rtmr3")?;
    let n = f.write(digest).context("extend rtmr3")?;
    ensure!(n == 48, "short write to rtmr3");
    Ok(())
}

/// A TD quote (v4) whose REPORTDATA is `report_data`.
pub fn quote(report_data: &[u8; 64]) -> Result<Vec<u8>> {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = format!("{TSM}/q{}", N.fetch_add(1, Ordering::Relaxed));
    fs::create_dir(&dir).with_context(|| format!("mkdir {dir}"))?;
    let result = (|| {
        let provider = fs::read_to_string(format!("{dir}/provider"))?;
        ensure!(provider.trim() == "tdx_guest", "unexpected TSM provider {provider:?}");
        let generation = || -> Result<u64> { Ok(fs::read_to_string(format!("{dir}/generation"))?.trim().parse()?) };
        let g0 = generation()?;
        fs::write(format!("{dir}/inblob"), report_data)?;
        ensure!(generation()? == g0 + 1, "concurrent configfs-tsm writer");
        let out = fs::read(format!("{dir}/outblob")).context("read outblob")?;
        ensure!(generation()? == g0 + 1, "concurrent configfs-tsm writer");
        // The outblob is zero-padded; a v4 quote is 0x27C bytes plus its
        // signature data, whose length is the u32 at 0x278.
        ensure!(out.len() >= 0x27C, "quote too short");
        let sig_len = u32::from_le_bytes(out[0x278..0x27C].try_into().unwrap()) as usize;
        ensure!(out.len() >= 0x27C + sig_len, "truncated quote");
        Ok(out[..0x27C + sig_len].to_vec())
    })();
    let _ = fs::remove_dir(&dir);
    result
}

/// The CC event log written by the firmware, without trailing 0xFF padding.
pub fn ccel() -> Vec<u8> {
    let mut log = fs::read(CCEL).unwrap_or_default();
    while log.last() == Some(&0xFF) {
        log.pop();
    }
    log
}
