//! Minimal TCG2 crypto-agile event log parser for the TDX CCEL.
//!
//! Record 0 is a TCG_PCR_EVENT (SHA-1 layout, the Spec ID event); every
//! later record is a TCG_PCR_EVENT2. In the CCEL the "PCR" field is the CC
//! measurement-register index: 0 = MRTD, 1..4 = RTMR0..3.

pub struct Event {
    pub mr: u32,
    pub etype: u32,
    pub sha384: Option<[u8; 48]>,
    pub data: Vec<u8>,
}

pub const EV_NO_ACTION: u32 = 0x3;

fn u16le(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}
fn u32le(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}
fn u64le(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

fn digest_len(alg: u16) -> Option<usize> {
    match alg {
        0x04 => Some(20), // SHA-1
        0x0B => Some(32), // SHA-256
        0x0C => Some(48), // SHA-384
        0x0D => Some(64), // SHA-512
        0x12 => Some(32), // SM3-256
        _ => None,
    }
}

/// Parse until the 0xFF padding, a zero header, or a truncated record.
/// Returns the events and, if parsing stopped early on garbage, why.
pub fn parse(log: &[u8]) -> (Vec<Event>, Option<String>) {
    let mut ev = Vec::new();
    let (Some(mr), Some(etype), Some(size)) = (u32le(log, 0), u32le(log, 4), u32le(log, 28)) else {
        return (ev, Some("log shorter than one TCG_PCR_EVENT".into()));
    };
    let Some(data) = log.get(32..32 + size as usize) else {
        return (ev, Some("truncated Spec ID event".into()));
    };
    ev.push(Event { mr, etype, sha384: None, data: data.to_vec() });
    let mut off = 32 + size as usize;
    loop {
        let (Some(mr), Some(etype), Some(count)) = (u32le(log, off), u32le(log, off + 4), u32le(log, off + 8)) else {
            return (ev, None); // clean end of buffer
        };
        if mr == u32::MAX || etype == u32::MAX || (mr == 0 && etype == 0 && count == 0) {
            return (ev, None);
        }
        if count == 0 || count > 8 {
            return (ev, Some(format!("bad digest count {count} at {off:#x}")));
        }
        let mut p = off + 12;
        let mut sha384 = None;
        for _ in 0..count {
            let Some(alg) = u16le(log, p) else { return (ev, Some(format!("truncated at {p:#x}"))) };
            let Some(n) = digest_len(alg) else { return (ev, Some(format!("unknown alg {alg:#x} at {p:#x}"))) };
            let Some(d) = log.get(p + 2..p + 2 + n) else { return (ev, Some(format!("truncated at {p:#x}"))) };
            if alg == 0x0C {
                sha384 = Some(d.try_into().unwrap());
            }
            p += 2 + n;
        }
        let Some(size) = u32le(log, p) else { return (ev, Some(format!("truncated at {p:#x}"))) };
        let Some(data) = log.get(p + 4..p + 4 + size as usize) else {
            return (ev, Some(format!("event data truncated at {p:#x}")));
        };
        ev.push(Event { mr, etype, sha384, data: data.to_vec() });
        off = p + 4 + size as usize;
    }
}

pub fn type_name(t: u32) -> String {
    match t {
        0x0000_0003 => "EV_NO_ACTION",
        0x0000_0004 => "EV_SEPARATOR",
        0x0000_0005 => "EV_ACTION",
        0x0000_0006 => "EV_EVENT_TAG",
        0x0000_0007 => "EV_S_CRTM_CONTENTS",
        0x0000_0008 => "EV_S_CRTM_VERSION",
        0x0000_000A => "EV_PLATFORM_CONFIG_FLAGS",
        0x0000_000D => "EV_IPL",
        0x8000_0001 => "EV_EFI_VARIABLE_DRIVER_CONFIG",
        0x8000_0002 => "EV_EFI_VARIABLE_BOOT",
        0x8000_0003 => "EV_EFI_BOOT_SERVICES_APPLICATION",
        0x8000_0004 => "EV_EFI_BOOT_SERVICES_DRIVER",
        0x8000_0005 => "EV_EFI_RUNTIME_SERVICES_DRIVER",
        0x8000_0006 => "EV_EFI_GPT_EVENT",
        0x8000_0007 => "EV_EFI_ACTION",
        0x8000_0008 => "EV_EFI_PLATFORM_FIRMWARE_BLOB",
        0x8000_0009 => "EV_EFI_HANDOFF_TABLES",
        0x8000_000A => "EV_EFI_PLATFORM_FIRMWARE_BLOB2",
        0x8000_000B => "EV_EFI_HANDOFF_TABLES2",
        0x8000_000C => "EV_EFI_VARIABLE_BOOT2",
        0x8000_00E0 => "EV_EFI_VARIABLE_AUTHORITY",
        _ => return format!("EV_{t:#x}"),
    }
    .to_string()
}

fn ascii(b: &[u8]) -> Option<String> {
    let b = b.strip_suffix(&[0]).unwrap_or(b);
    (!b.is_empty() && b.iter().all(|c| (0x20..0x7f).contains(c))).then(|| String::from_utf8_lossy(b).into_owned())
}

fn utf16(b: &[u8]) -> String {
    let u: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    String::from_utf16_lossy(&u)
}

/// A short human rendering of the event data.
pub fn describe(e: &Event) -> String {
    let d = &e.data;
    match e.etype {
        // UEFI_VARIABLE_DATA: GUID, u64 name chars, u64 data len, name, data
        0x8000_0001 | 0x8000_0002 | 0x8000_000C | 0x8000_00E0 if d.len() >= 32 => {
            let (n, len) = (u64le(d, 16).unwrap_or(0) as usize, u64le(d, 24).unwrap_or(0));
            let name = d.get(32..32 + 2 * n).map(utf16).unwrap_or_default();
            format!("var={name} var_data_len={len}")
        }
        // UEFI_IMAGE_LOAD_EVENT: addr, length, link addr, device-path length, path
        0x8000_0003 | 0x8000_0004 | 0x8000_0005 if d.len() >= 32 => {
            format!("image_len={} devpath_len={}", u64le(d, 8).unwrap_or(0), u64le(d, 24).unwrap_or(0))
        }
        // *_BLOB2 / HANDOFF_TABLES2: u8 description size, description, ...
        0x8000_000A | 0x8000_000B if !d.is_empty() => {
            let n = d[0] as usize;
            format!("desc={:?}", d.get(1..1 + n).map(String::from_utf8_lossy).unwrap_or_default())
        }
        _ => match ascii(d) {
            Some(s) => format!("ascii={s:?}"),
            None if d.len() <= 64 => format!("hex={}", crate::sha::hex(d)),
            None => String::new(),
        },
    }
}
