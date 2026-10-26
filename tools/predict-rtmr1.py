#!/usr/bin/env python3
"""Predict the GCE TDX RTMR1 of a vcontainer image, offline.

usage: predict-rtmr1.py BOOTX64.EFI disk.raw [PROBE_SERIAL_LOG]

On GCE C3 (Google TDVF) RTMR1 is the replay of exactly these CCEL MR-2 events:

  EV_EFI_ACTION                     "Calling EFI Application from Boot Option"
  EV_SEPARATOR                      00000000
  EV_EFI_GPT_EVENT                  UEFI_GPT_DATA of the boot disk
  EV_EFI_BOOT_SERVICES_APPLICATION  Authenticode SHA-384 of \\EFI\\BOOT\\BOOTX64.EFI
  EV_EFI_ACTION                     "Exit Boot Services Invocation"
  EV_EFI_ACTION                     "Exit Boot Services Returned with Success"

  RTMR1 = fold(lambda r, d: SHA384(r || d), digests, 48 * b"\\0")

Every digest except the application's is SHA-384 of the event data.
UEFI_GPT_DATA (packed) = the 92-byte primary GPT header (LBA 1)
  || u64le number of non-empty entries || those 128-byte entries in order.

With a probe serial log (tools/tdx-probe output), the script also decodes the
CCEL from the CCEL_B64: line, compares every MR-2 event with the prediction,
replays the log, and compares with the rtmr1 value the probe read from sysfs.
Python standard library only.
"""
import base64
import hashlib
import re
import struct
import sys

ZERO = bytes(48)
CALLING = b"Calling EFI Application from Boot Option"
EBS_INV = b"Exit Boot Services Invocation"
EBS_OK = b"Exit Boot Services Returned with Success"
EBS_FAIL = b"Exit Boot Services Returned with Failure"


def sha384(b):
    return hashlib.sha384(b).digest()


def extend(digests):
    r = ZERO
    for d in digests:
        r = sha384(r + d)
    return r


def authenticode(pe, alg="sha384"):
    """PE/COFF Authenticode digest, as EDK2's MeasurePeImage computes it."""
    u16 = lambda o: struct.unpack_from("<H", pe, o)[0]  # noqa: E731
    u32 = lambda o: struct.unpack_from("<I", pe, o)[0]  # noqa: E731
    if pe[:2] != b"MZ":
        raise ValueError("not a PE file (no MZ)")
    nt = u32(0x3C)
    if pe[nt:nt + 4] != b"PE\0\0":
        raise ValueError("not a PE file (no PE signature)")
    coff = nt + 4
    nsec, opt_size = u16(coff + 2), u16(coff + 16)
    opt = coff + 20
    magic = u16(opt)
    if magic == 0x20B:      # PE32+
        ndirs_off, dirs = opt + 108, opt + 112
    elif magic == 0x10B:    # PE32
        ndirs_off, dirs = opt + 92, opt + 96
    else:
        raise ValueError("bad optional header magic %#x" % magic)
    checksum = opt + 64
    size_of_headers = u32(opt + 60)
    ndirs = u32(ndirs_off)
    h = hashlib.new(alg)
    h.update(pe[:checksum])
    if ndirs <= 4:          # no certificate-table directory entry
        h.update(pe[checksum + 4:size_of_headers])
        cert_size = 0
    else:
        cert_dir = dirs + 4 * 8
        h.update(pe[checksum + 4:cert_dir])
        h.update(pe[cert_dir + 8:size_of_headers])
        cert_size = u32(cert_dir + 4)
    hashed = size_of_headers
    secs = []
    for i in range(nsec):
        s = opt + opt_size + 40 * i
        raw_size, raw_ptr = u32(s + 16), u32(s + 20)
        if raw_size:
            secs.append((raw_ptr, raw_size))
    for ptr, size in sorted(secs):
        h.update(pe[ptr:ptr + size])
        hashed += size
    if len(pe) > cert_size + hashed:
        h.update(pe[hashed:len(pe) - cert_size])
    return h.digest()


def gpt_event_data(disk_path):
    """UEFI_GPT_DATA as EDK2's Tcg2MeasureGptTable builds it."""
    with open(disk_path, "rb") as f:
        f.seek(512)
        lba1 = f.read(512)
        if lba1[:8] != b"EFI PART":
            raise ValueError("no primary GPT header at LBA 1")
        hdr_size = struct.unpack_from("<I", lba1, 12)[0]
        entries_lba, n, esize = struct.unpack_from("<QII", lba1, 72)
        f.seek(entries_lba * 512)
        table = f.read(n * esize)
    header = lba1[:hdr_size]
    used = [table[i * esize:(i + 1) * esize] for i in range(n)
            if table[i * esize:i * esize + 16] != bytes(16)]
    return header + struct.pack("<Q", len(used)) + b"".join(used)


EVENT_TYPES = {
    0x3: "EV_NO_ACTION", 0x4: "EV_SEPARATOR", 0x5: "EV_ACTION", 0x6: "EV_EVENT_TAG",
    0xD: "EV_IPL", 0xA: "EV_PLATFORM_CONFIG_FLAGS",
    0x80000001: "EV_EFI_VARIABLE_DRIVER_CONFIG", 0x80000002: "EV_EFI_VARIABLE_BOOT",
    0x80000003: "EV_EFI_BOOT_SERVICES_APPLICATION", 0x80000004: "EV_EFI_BOOT_SERVICES_DRIVER",
    0x80000006: "EV_EFI_GPT_EVENT", 0x80000007: "EV_EFI_ACTION",
    0x8000000A: "EV_EFI_PLATFORM_FIRMWARE_BLOB2", 0x8000000B: "EV_EFI_HANDOFF_TABLES2",
    0x800000E0: "EV_EFI_VARIABLE_AUTHORITY",
}
ALG_SIZES = {0x4: 20, 0xB: 32, 0xC: 48, 0xD: 64, 0x12: 32}


def parse_ccel(log):
    """TCG2 crypto-agile log -> [(mr, type, sha384 or None, data)]."""
    events = []
    # First record: TCG_PCR_EVENT (SHA-1 format), the Spec ID event.
    mr, etype = struct.unpack_from("<II", log, 0)
    size = struct.unpack_from("<I", log, 28)[0]
    events.append((mr, etype, None, log[32:32 + size]))
    off = 32 + size
    while off + 12 <= len(log):
        mr, etype, count = struct.unpack_from("<III", log, off)
        if mr == 0xFFFFFFFF or etype == 0xFFFFFFFF or count == 0 or count > 8:
            break
        p = off + 12
        d384 = None
        for _ in range(count):
            alg = struct.unpack_from("<H", log, p)[0]
            n = ALG_SIZES.get(alg)
            if n is None:
                raise ValueError("unknown digest algorithm %#x at %#x" % (alg, off))
            if alg == 0xC:
                d384 = log[p + 2:p + 2 + n]
            p += 2 + n
        size = struct.unpack_from("<I", log, p)[0]
        data = log[p + 4:p + 4 + size]
        if len(data) != size:
            break
        events.append((mr, etype, d384, data))
        off = p + 4 + size
    return events


def main():
    if len(sys.argv) not in (3, 4):
        sys.exit(__doc__)
    pe = open(sys.argv[1], "rb").read()
    app = authenticode(pe)
    gpt = gpt_event_data(sys.argv[2])
    expected = [
        ("EV_EFI_ACTION", sha384(CALLING), CALLING),
        ("EV_SEPARATOR", sha384(bytes(4)), bytes(4)),
        ("EV_EFI_GPT_EVENT", sha384(gpt), gpt),
        ("EV_EFI_BOOT_SERVICES_APPLICATION", app, None),
        ("EV_EFI_ACTION", sha384(EBS_INV), EBS_INV),
        ("EV_EFI_ACTION", sha384(EBS_OK), EBS_OK),
    ]
    rtmr1 = extend(d for _, d, _ in expected)
    # EDK2 variant: one failed ExitBootServices (stale MapKey) before success.
    alt = extend([expected[0][1], expected[1][1], expected[2][1], expected[3][1],
                  sha384(EBS_FAIL), expected[4][1], expected[5][1]])
    print("pe_sha256            %s" % hashlib.sha256(pe).hexdigest())
    print("pe_authenticode_sha384 %s" % app.hex())
    print("gpt_event_len        %d" % len(gpt))
    print("gpt_event_sha384     %s" % sha384(gpt).hex())
    print("rtmr1_predicted      %s" % rtmr1.hex())
    print("rtmr1_alt_ebs_retry  %s" % alt.hex())
    if len(sys.argv) == 3:
        return 0

    text = open(sys.argv[3], "rb").read().decode("utf-8", "replace")
    m = re.search(r"CCEL_B64: ?([A-Za-z0-9+/=]+)", text)
    q = re.search(r"MR rtmr1:sha384 ([0-9a-f]{96})", text)
    if not m:
        print("no CCEL_B64 line in %s" % sys.argv[3])
        return 1
    events = parse_ccel(base64.b64decode(m.group(1)))
    mr2 = [e for e in events if e[0] == 2 and e[1] != 0x3]
    ok = len(mr2) == len(expected)
    print("observed MR2 events  %d (expected %d)" % (len(mr2), len(expected)))
    for i, (_, etype, d, data) in enumerate(mr2):
        name = EVENT_TYPES.get(etype, hex(etype))
        want = expected[i] if i < len(expected) else ("<none>", b"", None)
        good = name == want[0] and d == want[1]
        ok &= good
        print("  %-34s %s len=%-4d %s" % (name, d.hex()[:16], len(data), "MATCH" if good else
                                          "DIFF (want %s %s)" % (want[0], want[1].hex()[:16])))
    replay = extend(d for _, _, d, _ in mr2)
    print("observed RTMR1 replay %s" % replay.hex())
    if q:
        print("observed RTMR1 sysfs  %s" % q.group(1))
        ok &= q.group(1) == rtmr1.hex()
    ok &= replay == rtmr1
    print("RTMR1 prediction %s" % ("MATCHES the observed value" if ok else "DOES NOT MATCH"))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
