//! Reading the firmware's CC event log, for diagnostics only: the verdict
//! rests on the quoted registers, not on this (unauthenticated) log.

use vcontainer_core::hex;

/// Lists the events the log claims were extended into RTMR `mr - 1`.
pub fn describe(log: &[u8], mr: u32) -> String {
    let mut out = String::new();
    for (index, kind, digest, data) in events(log) {
        if index == mr {
            let text: String = data.iter().map(|&c| if (0x20..0x7f).contains(&c) { c as char } else { '.' }).take(60).collect();
            out += &format!("  RTMR{} type {kind:#x} {}… {text}\n", mr - 1, &hex(digest)[..24]);
        }
    }
    if out.is_empty() {
        out = "  (no events in the CC event log)\n".into();
    }
    out
}

/// TCG2 crypto-agile records with a SHA-384 digest: (MR index, type, digest, data).
fn events(log: &[u8]) -> Vec<(u32, u32, &[u8], &[u8])> {
    let mut out = Vec::new();
    let u32_at = |o: usize| -> Option<u32> { Some(u32::from_le_bytes(log.get(o..o + 4)?.try_into().ok()?)) };
    // The first record is a TCG_PCR_EVENT (SHA-1 sized) header.
    let Some(size) = u32_at(28) else { return out };
    let mut o = 32 + size as usize;
    while let (Some(index), Some(kind), Some(count)) = (u32_at(o), u32_at(o + 4), u32_at(o + 8)) {
        let mut p = o + 12;
        let mut digest: &[u8] = &[];
        for _ in 0..count {
            let Some(alg) = log.get(p..p + 2).map(|b| u16::from_le_bytes([b[0], b[1]])) else { return out };
            let len = match alg {
                0x0004 => 20,
                0x000B => 32,
                0x000C => 48,
                0x000D => 64,
                _ => return out,
            };
            if alg == 0x000C {
                digest = log.get(p + 2..p + 2 + len).unwrap_or(&[]);
            }
            p += 2 + len;
        }
        let Some(dlen) = u32_at(p) else { return out };
        let Some(data) = log.get(p + 4..p + 4 + dlen as usize) else { return out };
        out.push((index, kind, digest, data));
        o = p + 4 + dlen as usize;
    }
    out
}
