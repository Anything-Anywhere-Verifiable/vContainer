//! Reading the firmware's CC event log, for diagnostics only: the verdict
//! rests on the quoted registers, not on this (unauthenticated) log.

use vcontainer_core::hex;

/// Lists the events the log claims were extended into RTMR `mr - 1`.
pub fn describe(log: &[u8], mr: u32) -> String {
    let mut out = String::new();
    for (index, kind, digest, data) in events(log) {
        if index == mr {
            let text: String = data.iter().map(|&c| if (0x20..0x7f).contains(&c) { c as char } else { '.' }).take(60).collect();
            let fingerprint = if digest.len() == 48 { format!("{}…", &hex(digest)[..24]) }
                else { "(no SHA-384 digest)".into() };
            out += &format!("  RTMR{} type {kind:#x} {fingerprint} {text}\n", mr.saturating_sub(1));
        }
    }
    if out.is_empty() {
        out = "  (no events in the CC event log)\n".into();
    }
    out
}

/// TCG2 crypto-agile records: (MR index, type, optional SHA-384 digest, data).
/// Stop at an incomplete or unsupported record, preserving complete diagnostics.
fn events(log: &[u8]) -> Vec<(u32, u32, &[u8], &[u8])> {
    fn take<'a>(log: &'a [u8], cursor: &mut usize, len: usize) -> Option<&'a [u8]> {
        let end = cursor.checked_add(len)?;
        let bytes = log.get(*cursor..end)?;
        *cursor = end;
        Some(bytes)
    }
    fn u32_at(log: &[u8], cursor: &mut usize) -> Option<u32> {
        Some(u32::from_le_bytes(take(log, cursor, 4)?.try_into().ok()?))
    }
    let mut out = Vec::new();
    // The first record is a TCG_PCR_EVENT (SHA-1 sized) header.
    let mut cursor = 28;
    let Some(size) = u32_at(log, &mut cursor) else { return out };
    if take(log, &mut cursor, size as usize).is_none() { return out; }
    while cursor < log.len() {
        let (Some(index), Some(kind), Some(count)) = (u32_at(log, &mut cursor), u32_at(log, &mut cursor), u32_at(log, &mut cursor)) else { return out };
        let mut digest: &[u8] = &[];
        for _ in 0..count {
            let Some(alg) = take(log, &mut cursor, 2).map(|b| u16::from_le_bytes([b[0], b[1]])) else { return out };
            let len = match alg {
                0x0004 => 20,
                0x000B => 32,
                0x000C => 48,
                0x000D => 64,
                _ => return out,
            };
            let Some(bytes) = take(log, &mut cursor, len) else { return out };
            if alg == 0x000C { digest = bytes; }
        }
        let Some(dlen) = u32_at(log, &mut cursor) else { return out };
        let Some(data) = take(log, &mut cursor, dlen as usize) else { return out };
        out.push((index, kind, digest, data));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(digests: &[(u16, Vec<u8>)], data: &[u8]) -> Vec<u8> {
        let mut log = vec![0u8; 32];
        for word in [2u32, 0x8000_0003, digests.len() as u32] { log.extend_from_slice(&word.to_le_bytes()); }
        for (algorithm, digest) in digests {
            log.extend_from_slice(&algorithm.to_le_bytes());
            log.extend_from_slice(digest);
        }
        log.extend_from_slice(&(data.len() as u32).to_le_bytes());
        log.extend_from_slice(data);
        log
    }

    #[test]
    fn missing_sha384_is_a_diagnostic_instead_of_a_panic() {
        for digests in [vec![], vec![(0x000B, vec![0x12; 32])]] {
            let description = describe(&event(&digests, b"untrusted event"), 2);
            assert!(description.contains("no SHA-384 digest"));
            assert!(description.contains("untrusted event"));
        }
    }

    #[test]
    fn truncated_records_are_safe_and_complete_records_are_described() {
        let log = event(&[(0x000C, vec![0xab; 48])], b"boot image");
        for end in 0..log.len() {
            assert!(describe(&log[..end], 2).contains("no events in the CC event log"), "prefix {end}");
        }
        let description = describe(&log, 2);
        assert!(description.contains("abababababababababababab"));
        assert!(description.contains("boot image"));
    }

    #[test]
    fn a_truncated_tail_does_not_hide_complete_events() {
        let mut log = event(&[(0x000C, vec![0xab; 48])], b"complete event");
        log.extend_from_slice(&[2, 0, 0, 0, 0xff]);
        assert!(describe(&log, 2).contains("complete event"));
    }

    #[test]
    fn oversized_header_lengths_are_safe_diagnostics() {
        let mut log = vec![0u8; 32];
        log[28..32].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(describe(&log, 2).contains("no events in the CC event log"));
    }
}
