# vContainer boot image

This is the VM's **boot image**, not your agent's container image. For normal
setup and a first deployment, start with the [main README](../README.md#install).
The commands here run from the repository root.

## Normal build

On Linux x86-64 with Python 3.9+, a native C toolchain, Git, Bash, and Docker available
(the installer sets up Rust through rustup; `curl` and `sh` are needed if rustup is missing):

```sh
python3 scripts/install.py
```

This builds the boot image, installs the `vcontainer` CLI, and prints the expected
measurements. Use `--dev` for an uncommitted checkout. To also upload the image to
Google Cloud and save `~/.config/vcontainer/release.json`, use
`python3 scripts/install.py --publish --project PROJECT` with authenticated
`gcloud`. To build only the boot artifacts without installing the CLI, run
`scripts/build-image.sh` directly.

You do not need the hardware probe below to deploy an application. Rebuild the
boot image when vContainer changes; deploy a new application image with
`vcontainer deploy` without rebuilding the boot image.

## Image layout and build inputs

One reproducible PE file, `BOOTX64.EFI` (a Linux 6.18.57 EFI-stub kernel with a
built-in initramfs holding only `/init`, plus a built-in command line), on a
1 GiB GPT disk with a single ESP. Google's TDX firmware loads it from
`\EFI\BOOT\BOOTX64.EFI` and measures its Authenticode SHA-384 into RTMR1.
Design: `docs/design/DESIGN.md` §4.1, §4.2, §14.

| File | Role |
|---|---|
| `image/kernel.config` | miniconfig for `make allnoconfig KCONFIG_ALLCONFIG=…`; every addition is commented; section (4) lists the symbols that must stay off |
| `image/initramfs.list` | `gen_init_cpio` list of the built-in initramfs |
| `image/check-config.sh` | fails on any requested value missing from, or forbidden symbol present in, the final `.config` |
| `scripts/build-image.sh` | builds `vcontainer/builder:dev`, then `/init` and everything else in it; writes `out/` |
| `scripts/install.py` | normal build/install wrapper; optional cloud publication |
| `tools/tdx-probe/` | diagnostic M0 probe that replaces `/init` (`build.sh`, `gce-run.sh`); not needed for normal setup |
| `tools/predict-rtmr1.py` | offline RTMR1 prediction; checks it against a probe log |

## Optional hardware diagnostic probe

This workflow replaces the normal `/init` with a diagnostic program. The resulting
image cannot run application deployments. It is for checking platform behavior
and measurements on real TDX hardware.

Build an image with the probe as `/init`:

```sh
scripts/build-image.sh --dev --init "$(tools/tdx-probe/build.sh)"
```

Set your own project and existing bucket, then boot real TDX VMs and collect
their serial logs. This creates billable resources; the script deletes its VMs
and image afterwards, but retains the uploaded object. Choose a zone with C3
quota:

```sh
PROJECT=YOUR_PROJECT BUCKET=YOUR_BUCKET ZONE=us-central1-a tools/tdx-probe/gce-run.sh c3-standard-4 c3-standard-8
```

Check the offline RTMR1 prediction against a probe log:

```sh
python3 tools/predict-rtmr1.py out/BOOTX64.EFI out/disk.raw out/probe-serial-c3-standard-4.log
```

Rebuild the normal boot image with `python3 scripts/install.py --dev --clean`
before publishing an image for application use.

## Build notes

- Source = `.cache/linux-6.18.57.tar.xz` (sha256 `5bd9acdc…f98c`), verified
  whenever `.cache/linux-6.18.57` is extracted (`--clean` drops the tree).
- `KBUILD_BUILD_TIMESTAMP` = `SOURCE_DATE_EPOCH` = 2026-01-01 00:00:00 UTC
  (constant), `KBUILD_BUILD_{USER,HOST}=vcontainer`, `KBUILD_BUILD_VERSION=1`,
  `LOCALVERSION=` (no `+`). So `BOOTX64.EFI` depends only on the source.
- Disk: disk GUID `7C0E0000-5643-4F4E-5441-494E45520001`, ESP GUID
  `…-494E45520002`, name `vcontainer-esp`, LBA 2048..2097118, FAT32 volume id
  `7C0E5643`, `mkfs.vfat --invariant`, mtools timestamps from `SOURCE_DATE_EPOCH`.
- Reproducibility: two `--clean` builds (fresh `O=` tree) gave byte-identical
  `BOOTX64.EFI`, `disk.raw` and even `vcontainer.tar.gz`, both before and after
  the patch. The PE that ran on GCE (`sha256 5ca55270…3ad74`, built with
  `SOURCE_DATE_EPOCH=1792121823`) was rebuilt bit-for-bit afterwards.
- All 36 section (4) symbols are off without any fight: nothing in this
  config `select`s them. `allnoconfig` also silently drops default-on
  hardening (`CPU_MITIGATIONS`, which "cannot be overridden at runtime",
  `VMAP_STACK`, KASLR) and common syscalls (`madvise`, `membarrier`, `rseq`,
  `seccomp`, …); kernel.config re-enables them in sections (2) and (3).

## M0 results (GCE C3 TDX, us-central1-a, 2026-10-16)

Image `vcontainer-probe-20261016-035500` (and `-040829`, the same bytes);
`--no-shielded-secure-boot --shielded-vtpm`, no Secure Boot keys declared on the
image. Logs: `out/probe-serial.log` (= c3-standard-4), `out/probe-serial-c3-standard-8.log`,
`out/probe-attempt1-nopv-BUG.log` (first attempt, unpatched kernel).

| Check | c3-standard-4 | c3-standard-8 |
|---|---|---|
| Plain EFI-stub PE boots as `BOOTX64.EFI` | yes (after the x2APIC patch) | yes |
| Kernel start → `Run /init` | 1.24 s | 1.38 s |
| MRTD | `c1ee9c16e3afc506cfe042c5b846a368528f3b37618eafb27469bc114cf914e9222c91618470e7f2b28ac360968270a5` | same |
| RTMR0 | `c49d22af…552c6307` | `5b56e364…12779214` (TD HOB + ACPI differ) |
| RTMR1 | `243846d5162f8ca9311c74678a88f73f68aca4ea78009781613cd65f22bbc5372685bc4a79d08388c6e5630f9feddf6d` | same |
| RTMR1 = offline prediction | MATCH (event by event) | MATCH |
| RTMR2 | all zero | all zero |
| RTMR3 at boot | all zero | all zero |
| CCEL replay = RTMR0..2 (sysfs and quote) | MATCH ×3 | MATCH ×3 |
| MRCONFIGID / MROWNERCONFIG | zero / zero | zero / zero |
| MROWNER = SHA-384(PZID JSON) | MATCH | MATCH |
| TD attributes / XFAM | `0x10000000` (SEPT_VE_DISABLE) / `0x602e7` | same |
| TD_CTLS (kernel log) | PENDING_VE_DISABLE ENUM_TOPOLOGY VIRT_CPUID2 REDUCE_VE | same |
| Quote (configfs-tsm, provider `tdx_guest`) | v4, 4935 bytes (0x27C + 4299), 8000-byte outblob zero-padded, generation 0→1, ~40 ms, 3 PEM certs embedded, REPORTDATA = inblob | v4, 4935 bytes, ~42 ms |
| TEE_TCB_SVN | `0f010a00000000000000000000000000` | same |
| RTMR3 extend (48 bytes = SHA-384("vcontainer-probe")) | after = `377bd8cbb7d500bdb45d7e792e0c12bcf96e9e1dbf40e34e6ef572fb6c936ddf1fa2df383461b8d2d9d8975466af7b1b` = SHA-384(0⁴⁸‖digest): MATCH | same value, MATCH |
| kmsg `IP-Config` parse | `device=eth0`, `gw=10.128.0.1` | same |
| Routes | host route `10.128.0.1/32 dev eth0` added; default route already existed (EEXIST), see Surprises #2 | same |
| Metadata (`instance/id`, `project/numeric-project-id`, `instance/zone`) | HTTP 200 ×3 | HTTP 200 ×3 |
| Internet | TCP 1.1.1.1:80 connected; DNS + `http://connectivitycheck.gstatic.com/generate_204` → 204 | same |
| `/proc/net/pnp` | `#PROTO: DHCP`, `domain us-central1-a.c.agentlockbox.internal.`, `nameserver 169.254.169.254`, `bootserver 10.128.0.1` | same |
| DHCP MTU | 1460 (option 26, applied by IP_PNP) | same |
| `lo` | UP (`flags=0x9`, operstate `unknown`) | UP |

### RTMR1: observed event sequence (CCEL MR index 2)

| # | Event type | SHA-384 digest | Event data |
|---|---|---|---|
| 1 | `EV_EFI_ACTION` (0x80000007) | `77a0dab2312b4e1e57a84d865a21e5b2ee8d677a21012ada819d0a98988078d3d740f6346bfe0abaa938ca20439a8d71` | ASCII `Calling EFI Application from Boot Option` (40 bytes, no NUL) |
| 2 | `EV_SEPARATOR` (0x4) | `394341b7182cd227c5c6b07ef8000cdfd86136c4292b8e576573ad7ed9ae41019f5818b4b971c9effc60e1ad9f1289f0` | `00000000` |
| 3 | `EV_EFI_GPT_EVENT` (0x80000006) | `a6b3142eb73c97296a9354e34546264eabe320e1fd60de6642fdd85d3d44a360cacdb09bbbef20253b8a60bac6eb49e6` | **228 bytes** = 92-byte header + u64 count (1) + one 128-byte entry; byte-identical to the GPT of `out/disk.raw` |
| 4 | `EV_EFI_BOOT_SERVICES_APPLICATION` (0x80000003) | `910b101f0d67557e26f580cac130ad31997065a9361c1b68950a586ff865c2ffc48a63de494ab762e6e89aabfa2185ea` = Authenticode SHA-384 of `BOOTX64.EFI` | 160 bytes: image length 3981824 (= file size), device path `PciRoot(0)/Pci(4,0)/NVMe(1)/HD(1,GPT,<ESP GUID>,2048,2095071)/\EFI\BOOT\BOOTX64.EFI` |
| 5 | `EV_EFI_ACTION` | `214b0bef1379756011344877743fdc2a5382bac6e70362d624ccf3f654407c1b4badf7d8f9295dd3dabdef65b27677e0` | ASCII `Exit Boot Services Invocation` (29 bytes) |
| 6 | `EV_EFI_ACTION` | `0a2e01c85deae718a530ad8c6d20a84009babe6c8989269e950d8cf440c6e997695e64d455c4174a652cd080f6230b74` | ASCII `Exit Boot Services Returned with Success` (40 bytes) |

No other MR-2 events: no driver, no second application, no `EBS Failed`.
Identical on c3-standard-4 and c3-standard-8 (RTMR1 is shape-independent).
RTMR2 is all zero: the stub's own LoadOptions/initrd events went to the vTPM
(TCG2 present), and this image has no initrd and no systemd-stub sections.

### Predicting RTMR1 offline

```
H(x)    = SHA-384(x);  extend(r, d) = H(r || d);  r0 = 48 zero bytes
GPT     = LBA1[0:92]                       # primary GPT header (HeaderSize = 92)
          || u64le(n)                      # number of entries with a non-zero type GUID
          || those n 128-byte entries      # in table order (here n = 1 → 228 bytes)
APP     = Authenticode-SHA-384(BOOTX64.EFI)
          # PE/COFF Authenticode as EDK2 MeasurePeImage: hash the headers up to
          # SizeOfHeaders minus the CheckSum field and the Certificate Table
          # directory entry, then each section's raw data sorted by
          # PointerToRawData, then any trailing bytes minus the certificate table.
RTMR1   = fold(extend, [H("Calling EFI Application from Boot Option"), H(00000000),
                        H(GPT), APP,
                        H("Exit Boot Services Invocation"),
                        H("Exit Boot Services Returned with Success")], r0)
```

`tools/predict-rtmr1.py BOOTX64.EFI disk.raw [LOG]` implements this with the
standard library only, prints both legal values (normal, and EDK2's
`Exit Boot Services Returned with Failure` retry variant, never observed), and
with a probe log decodes `CCEL_B64:` and compares every MR-2 event: all six
match on both shapes and the replay equals the sysfs RTMR1. The GPT header
depends only on the image (AlternateLBA 2097151 for the 1 GiB image even on the
10 GB boot disk). `build-image.sh` writes the prediction to `out/rtmr1.txt`.

### RTMR0 (for reference; not pinned)

MR index 1, 14 measured events: `EV_EFI_HANDOFF_TABLES2 "TdxTable"` (TD HOB;
c3-standard-4 `d587edd1…`, c3-standard-8 `39e89cd8…`, the values FACTS-tdx.md
predicted), `EV_EFI_PLATFORM_FIRMWARE_BLOB2 Fv(48DB5E17…)` (CFV `4e1221cf…`),
5× `EV_EFI_VARIABLE_DRIVER_CONFIG` (SecureBoot=0 `cfa4e2c6…`, PK 806 B, KEK
3066 B, db 6133 B, dbx 11936 B: GCE's default keys, because the image declares
none), `EV_SEPARATOR`, 3× `EV_PLATFORM_CONFIG_FLAGS "ACPI DATA"` (c3-standard-4
`f60c35e5/509dcfe1/0bb0afa0`, c3-standard-8 `4f05c335/4274fedf/89b98ddb`, both
already seen in Oct 2026), and `EV_EFI_VARIABLE_BOOT` BootOrder `7210af19…`,
Boot0001 `a25333c7…` ("UEFI nvme_card-pd"), Boot0000 `23ada07f…`. No
`EV_EFI_VARIABLE_AUTHORITY` (Secure Boot off). MR 0 carries two unmeasured
`EV_NO_ACTION` SP800-155 Event3 records (Google, Inc. / Google Compute Engine).

## Surprises

1. **`nopv` breaks TDX boot on stock 6.18.57.** The first boot (no patch) hit
   `Kernel BUG at setup_IO_APIC+0x7f` and looped (`panic=-1`), see
   `out/probe-attempt1-nopv-BUG.log`. A TD has a locked x2APIC and no IRQ
   remapping; Linux accepts that only through `x86_init.hyper.x2apic_available()`,
   which only the KVM (or VMware/Hyper-V/…) hypervisor hook provides. With
   `nopv` (or `KVM_GUEST=n`) no hypervisor is detected, `try_to_enable_x2apic()`
   cannot disable the locked x2APIC, the APIC-ID limit stays at 32767,
   `x86_vector_select()` refuses the IO-APIC, and `BUG_ON(mp_irqdomain_create())`
   fires. `image/patches/0001-…` (+16 lines in `arch/x86/coco/tdx/tdx.c`) sets
   the hook to `true` for TDX guests, which limits APIC IDs to 255 (physical
   x2APIC mode; `APIC: Switched APIC routing to: physical x2apic`). Without
   KVM's MSI extended-destination-ID feature, shapes with APIC IDs ≥ 255 would
   lose CPUs; only c3-standard-4/8 were tested.
   **Decision (2026-10-16):** no kernel patch. The image uses `KVM_GUEST=y`
   without `nopv`: the host's default configuration (like its ACPI tables) is
   trusted, KVM's hook covers large shapes, and TDX keeps the clocksource on
   `tsc`. The patch and the `nopv` probe logs are kept only in git history.
2. **Kernel DHCP and GCE's /32: the default route appears by accident.** IP_PNP
   uses 255.255.255.255 as its "no netmask" sentinel, so GCE's /32 is treated as
   absent: `IP-Config: Guessing netmask 255.0.0.0`. The guessed classful /8 puts
   the gateway 10.128.0.1 "on link", so the kernel itself installs
   `10.0.0.0/8 dev eth0` and `default via 10.128.0.1`; the probe's
   `SIOCADDRT` default route got EEXIST and its `/32` host route was redundant
   but harmless. Metadata and the Internet worked. /init must not rely on this
   (a subnet outside 10/8 would get a different classful guess, and the /8
   on-link route is wrong for the VPC): add `gw/32` + default idempotently,
   treat EEXIST as success, and delete the bogus classful route.
3. **The DHCP host name overrides `CONFIG_DEFAULT_HOSTNAME`.** IP_PNP copied
   option 12 into the UTS nodename, truncated to 64 characters
   (`vcontainer-probe-20261016-035500-c3standard4.us-central1-a.c.ag`).
   DESIGN.md says the host name is not taken from DHCP; /init has to reset it.
4. **The CCEL Spec ID record has PCRIndex 1, not 0.** Parsers must not assume
   0; replay skips `EV_NO_ACTION` anyway. Log: 23 records, 25047 bytes used of a
   256 KiB area (rest 0xFF).
5. **GCE's default Secure Boot keys are in RTMR0** when the image declares
   none (PK/KEK/db from Google/Microsoft, the 11936-byte dbx), even with Secure
   Boot off. RTMR0 therefore follows whatever Google enrolls by default.
6. `ACPI BIOS Warning (bug): Incorrect checksum in table [APIC]` on every boot
   (host-generated MADT).
7. SWIOTLB takes 1 GiB on c3-standard-4 (6 % of RAM rounded up), the largest
   single memory cost of the guest.
8. In QEMU (no NIC driver) IP_PNP gives up before bringing `lo` up, so `lo`
   stays DOWN there; on GCE it is UP only because IP_PNP succeeded. /init should
   bring `lo` up itself.
9. Kernel → `/init` is 1.2–1.4 s; IP_PNP DHCP completes before `/init` starts
   (≈1.2 s). The quote takes ≈40 ms.

## Open

- Resolved: `KVM_GUEST=y` without `nopv` (see Surprises #1).
- Resolved: `/init` keeps IP_PNP and adds the routes itself, treating EEXIST
  as success, and resets the hostname.
- RTMR0 is not appraised: the host's default configuration is trusted.
