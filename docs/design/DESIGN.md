> **Historical design study (2026-10-15), written when the project was called ccrun.** [../whitepaper.md](../whitepaper.md) describes what vcontainer actually implements and is authoritative. Points superseded by the implementation: SNP is not implemented; the host's default configuration is trusted, so there is no ACPI allowlist, no BadAML patch and no RTMR0 appraisal; the kernel uses `KVM_GUEST=y` without `nopv`; `ip=dhcp` plus routes from the kernel log replace the DHCP client; `ccrun` is `vcontainer`.

# ccrun: a minimal confidential container runtime on GCP (TDX + SNP), design draft v0.3

> Status: design v0.3 (2026-10-15), not implemented. v0.1 (not included in this repository) went through an adversarial review by Codex ([REVIEW-codex-design-r1.md](REVIEW-codex-design-r1.md)) and by Claude from four perspectives (each finding was re-checked by a rebutter), and was revised into v0.2. After a second Codex review round ([REVIEW-codex-design-r2.md](REVIEW-codex-design-r2.md)) it was revised again into v0.3. [REVIEW.md](REVIEW.md) records how each finding was handled.
> Factual basis: the research notes on TDX and SNP (not included in this repository). Each fact in them is marked as verified or inferred and cites its source: source code line numbers, Google documentation, and data the author collected on real hardware.

---

## 0. One-page summary

- **Shape**: a single, reproducibly built **PE file**, made of a Linux EFI stub kernel, a built-in initramfs and a built-in cmdline. The initramfs holds only one static Rust binary, `/init`. The PE sits on a GPT disk that has only an ESP partition, as `\EFI\BOOT\BOOTX64.EFI`, and Google's firmware loads it directly. No GRUB, shim, systemd-stub, rootfs disk, containerd, runc or systemd is needed.
- **One image, three modes**:

| `--tee` | Machine type | Root of trust for the pre-pull state | Requirement 3 met against a malicious host? |
|---|---|---|---|
| `tdx` (**recommended**) | C3 | Intel hardware: MRTD (with a Google-signed endorsement) + RTMR0–2 + RTMR3 ledger | **Yes**, provided one trusts the Google firmware binary (signed) and the host ACPI (pinned to a reviewed baseline, with the BadAML patch applied) |
| `snp-svsm` | N2D, needs the hidden alpha option `--svsm-config` | AMD hardware: MEASUREMENT covers OVMF and the SVSM; the vTPM runs inside the SVSM, and its manifest v1 is bound into REPORT_DATA | **Conditionally**: the M0 measurements must pass (§14), and one must trust the Google-signed SVSM binary |
| `snp-hostvtpm` | N2D, GA | **The entire Google host** (the VMM's TIS emulation + vTPM) | **No**. It only protects against non-host attackers. Without `allow_host_vtpm: true` in the policy, it never PASSes |

  GCE has no direct kernel boot and cannot set HOST_DATA or an ID block; SNP has no RTMRs; the GA vTPM runs on the host side. So on GA SNP, requirement 3 against a malicious host is **not achievable**. This is a platform limit. What ccrun can do is label it clearly.
- **Attest first, then deliver (attest-then-launch)**: after boot, the VM stops in `WAITING`. At this point it holds neither the agent image nor any secret. The owner's CLI first remotely verifies the complete pre-pull state: firmware, kernel, `/init`, host ACPI, and the ledger `[boot, owner]`. Only after this check passes does it send the launch bundle: the config in plaintext, signed by the owner; **only the secrets** are HPKE-encrypted to this boot's public key. On receipt, `/init` first records the launch in the ledger, enters STARTING and **stops**. The CLI fetches evidence once more, confirms that the ledger `[boot, owner, launch(config)]` matches what it expects, and then sends an owner-signed commit. Only after `/init` receives the commit does it pull by digest → verify and unpack each layer into tmpfs → execute. As a result, **the complete pre-pull state (including the measurement of the config) has been verified remotely before the pull begins**, rather than only inferred after the fact. The CLI does this step automatically; the user does not notice it.
- **Isolation does one thing**: it stops the workload from forging or corrupting ccrun's evidence. The means: Docker's default capability set minus `MKNOD` and `SYS_CHROOT`; all of init's mounts unmounted in a child mount namespace; `/proc/sys` read-only; no usermode helper, SysRq or coredump in the kernel. It is not a sandbox.
- **Code size**: about 3.5k lines of Rust (guest about 1.6k, CLI and verifier about 1.6k, shared about 0.3k), about 280 lines of scripts and configuration, plus one kernel patch of about 150 lines (BadAML). For comparison: the Confidential Space launcher is about 6.6k lines of Go (plus containerd and COS), Tinfoil about 16.8k lines of Go (plus docker), and dstack about 133k lines of Rust.
- **Usage**: one idempotent command does "create VM → verify pre-pull state → deliver → verify running state → print the endpoint": `ccrun up NAME --tee tdx --image REG/REPO@sha256:… -p 8080 --secret KEY=@file`. A third-party re-check needs only one self-contained policy file: `ccrun verify URL --policy NAME.policy.json`.

---

## 1. Goals, non-goals, threat model

### 1.1 Goals
1. **G1 Container format**: run any linux/amd64 OCI/Docker image. Manifest, index and schema2 are all supported; layers may be tar, gzip (including multi-member) or zstd. The semantics of entrypoint, cmd, env, workdir and user are followed, as are OCI whiteout and replacement semantics.
2. **G2 Least code, easiest to use**: one CLI, one init, one build script. Deployment and failure recovery use the same idempotent command.
3. **G3 Verifiable pre-pull state** (precise wording as of v0.2): before any byte of the agent image enters the VM, the verifier can confirm every **input that decides "what code runs, with what configuration"**: firmware, kernel, initramfs, cmdline, `/init`, host ACPI (AML is itself code), owner identity and launch config. The remaining host inputs fall into two classes: either measured code parses them as untrusted data (they are attack surface; see the list in §4.4), or they affect only timing or availability.
4. **G4 Rust**: both the guest and the verifier are written in Rust; the guest is a static musl binary. The only C code is musl libc and `ring`'s cryptographic primitives (C+asm), compiled in a version-pinned build container.

### 1.2 Non-goals
- No workload sandbox: the VM is the boundary, and it is single-tenant. D7 provides only the small amount of isolation needed to prevent forged evidence.
- Out of scope: multi-container orchestration, persistent volumes, live migration, GPUs (A3 H100 is deferred to v2), unattended secret re-delivery (in v1 this is done by the idempotent `ccrun up`).
- No dependency on Google Cloud Attestation. All verification happens locally.

### 1.3 Threat model
| Entity | Assumption |
|---|---|
| Intel CPU + TDX module; AMD CPU + ASP | Trusted, subject to TCB/SVN minimums |
| Intel PCS, AMD KDS | Trusted (as signing roots) |
| Google firmware (closed source, binary published) | **Trusted, but only with a Google-signed endorsement**: the MRTD or MEASUREMENT must appear in the signed contents |
| Google SVSM (`snp-svsm` mode) | Trusted, provided the endorsement's `svsm_measurement` covers it (binary not published) |
| Google host: VMM, ACPI, TD HOB, NVRAM, device emulation, metadata, DHCP, RTC, disk, network, serial port, keyboard | **Untrusted**. The only exception is `snp-hostvtpm`: there the whole host must be trusted, because the vTPM and the TIS MMIO emulation that carries all TPM traffic are both on the host side |
| Image registry, DNS | Untrusted. Integrity comes from the digest; credential confidentiality comes from WebPKI plus a time floor (§6.3) |
| Owner | Trusted; holds the Ed25519 owner key |
| Workload | Whether it is trusted is for the relying party to judge from the digest and config. ccrun only guarantees that "what runs is exactly this workload", and that it cannot forge ccrun's evidence |

The attacker has three goals: (a) make the VM run code, configuration or ACPI other than what is claimed; (b) obtain the secrets delivered at launch; (c) forge or tamper with evidence, including lying about the running state. Availability attacks are out of scope.

---

## 2. Key design decisions

| # | Decision | Rationale |
|---|---|---|
| D1 | **Single PE: kernel EFI stub + built-in initramfs + `CONFIG_CMDLINE_OVERRIDE` + `noinitrd`; `CONFIG_MODULES=n`** | GCE only loads `BOOTX64.EFI` from the disk and measures its **Authenticode** into RTMR1 (TDX) or vTPM PCR4 (SNP). Whenever TCG2 is present, the EFI stub measures an external initrd and LoadOptions **only** into the vTPM, not into RTMR2, so every kernel input must be inside the PE. The host controls NVRAM (the OptionalData of Boot####/BootNext), so `noinitrd` is essential: otherwise the host could inject an initrd through LoadFile2 or `initrd=` and override `/init`. LoadOptions are still measured (into the vTPM only), but the kernel does not use them |
| D2 | **No rootfs disk; the image lives in tmpfs**; the guest never writes to disk | TDX/SNP private memory itself provides confidentiality and integrity. The GPT is one event in RTMR1, and it stays deterministic as long as nothing writes to the disk. This is also why the kernel builds no NVMe, SCSI or ATA drivers |
| D3 | **The TDX ledger goes in RTMR3** (sysfs on Linux ≥6.16); **the vTPM is not used for measurement at all** | One quote covers everything. The GCE TDX vTPM also has a quirk: it records the PE in PCR2 |
| D4 | **The SNP ledger goes in vTPM PCR8**; the three modes share code (`snp-svsm` and `snp-hostvtpm` differ by only about 100 lines) | See §4.3 for the platform limits; `snp-hostvtpm` must be explicitly authorized before it can PASS |
| D5 | **Only one mode: attest-then-launch**; metadata holds only the owner public key, and it is measured before use | Agents almost always need secrets (API keys). Only "verify first, then deliver" guarantees that secrets go only to the boot that was verified. The host does not have the owner private key, so it cannot start a launch on the owner's behalf. If the host replaces the public key in metadata, ledger #1 will not match. The cost is that secrets must be delivered again after a reboot; the same idempotent `ccrun up` does this. Use cases that need no secrets can use the v1.1 metadata autostart (about 40 lines, §15) |
| D6 | **A self-written minimal OCI pull and unpack** (`ureq`, pull by digest only, per-layer pipeline, unpack in a chroot child process) | No containerd/runc/youki needed (youki is about 23k lines and brings C dependencies). The `ureq` approach measured about 46 crates and 1.8 MB; the `oci-client` approach is 156 crates and 3.7 MB, and it also needs tokio |
| D7 | **Only the minimal isolation needed to prevent evidence forgery** (details in §6.5). Kernel: no usermode helper, SysRq, coredump, user ns, io_uring, BPF or userfaultfd. Process: new mount and pid ns, all of init's mounts unmounted, `/proc/sys` read-only, capabilities set to the Docker default set minus `MKNOD` and `SYS_CHROOT`, stdin connected to `/dev/null` | The ledger is append-only and the verifier requires an exact replay, so the workload **already cannot change** "which config was launched". D7 protects against three other things: minting its own quote with arbitrary report_data (and so forging the runtime status); appending events to the ledger so that the evidence always fails verification; reading init's memory (which holds the registry credentials) |
| D8 | **The CLI calls `gcloud` directly** | No need to implement the GCE API and OAuth |
| D9 | **Secure Boot off; PK/KEK/db/dbx and the Boot variables are bound by digest only, and their values are not evaluated** | RTMR1 already pins the PE that was **actually loaded**, and the PE does not read LoadOptions. The kernel does not use UEFI keys either (`LOAD_UEFI_KEYS=n`, no modules, no kexec, `efi=noruntime`). By construction, these host-controlled variables cannot affect security. This also removes the false alarms caused by drift in Google's default dbx |
| D10 | **The kernel is configured with `make allnoconfig KCONFIG_ALLCONFIG=kernel.miniconfig`; after the build, a separate assertion script checks every must-be-on and must-be-off item** | Review found three problems: defconfig pulls in dozens of drivers that parse host devices; an `=n` written in a fragment is silently turned back on by `select` (e.g. `ATA→SCSI`); and when a dependency is left out (e.g. `VIRT_DRIVERS`), `olddefconfig` silently drops the TDX/SNP drivers |
| D11 | **The ACPI allowlist comes from a manually reviewed platform baseline package; interactive TOFU is removed; the BadAML patch is mandatory** | The ACPI digest in RTMR0 (or PCR1 on SNP) is computed over the raw fw_cfg blob, which the guest cannot reconstruct, and it drifts. Automatic enrollment would let the host decide, on the owner's behalf, which code the kernel executes |

---

## 3. Overall architecture

```
 owner / relying party                             GCE (C3 TDX or N2D SNP)
┌────────────────────────────────────┐   gcloud    ┌──────────────────────────────────────────────────────────────┐
│ ccrun (CLI + verifier)             │ ──────────▶ │ Google firmware (MRTD / SNP MEASUREMENT, signed endorsement) │
│  owner.ed25519                     │             │  └ ESP:\EFI\BOOT\BOOTX64.EFI   ← the only image              │
│  release.json (reproducible)       │             │     kernel EFI stub + built-in initramfs and cmdline         │
│  platform.json (reviewed baseline) │  HTTP :7700 │     └ /init (ccrun-init, Rust, static)                       │
│  NAME.policy.json (self-contained) │ ◀────────── │        ├ lo + DHCP + resolv.conf                             │
│  Intel PCS / AMD KDS cache         │ /v1/evidence│        ├ owner public key from metadata (measured first)     │
│  Google endorsement cache          │ ──────────▶ │        ├ ledger → RTMR3 / vTPM PCR8                          │
│                                    │ /v1/launch  │        ├ per layer: pull→verify→unpack (chroot helper)       │
└────────────────────────────────────┘             │        └ launch helper: new ns, unmount, exec handshake      │
                                                   └──────────────────────────────────────────────────────────────┘
```

```
ccrun/
  crates/core/   # ledger, schemas for evidence/launch/config/status, TCG2 log parsing and replay, digest binding per event type
  crates/init/   # guest PID1 (static musl); --stage unpack / --stage run are single-threaded helpers
  crates/cli/    # up / down / verify / measure / golden / logs / keygen / image publish
  image/kernel.fragment       # positive allowlist (about 120 lines)
  image/kernel.must-be-off    # symbols that must be n (asserted at build time)
  image/patches/0001-acpi-badaml-guard.patch   # about 150 lines; Flashbots, dstack and Contrast all carry this patch
  image/build.sh              # about 100 lines
  image/Containerfile         # pinned build environment: Rust, musl-gcc, kernel toolchain, mtools, gdisk
  platform/platform.json      # platform baseline reviewed and signed by the maintainer
```

---

## 4. Boot and measurement chain

### 4.1 Common to both

Built-in cmdline: `console=ttyS0 noinitrd nopv panic=-1 efi=noruntime random.trust_bootloader=off`
- `noinitrd`: see D1 for the reason.
- `nopv`: the host can claim to be VMware or Hyper-V through CPUID `0x4000_00xx` and trigger the matching PV paths (tsc_khz is decided by the host, and the steal-time page is in decrypted state). `nopv` turns these paths off.
- `efi=noruntime`: do not call the host firmware's runtime services.
- No `ip=dhcp`: GCE hands out a /32 netmask plus option 121 routes, and the kernel's IP_PNP does not understand option 121.

The initramfs is generated with `gen_init_cpio`, with every mtime set to `KBUILD_BUILD_TIMESTAMP`:
```
dir /dev 0755 0 0
nod /dev/console 0600 0 0 c 5 1
dir /proc 0755 0 0
dir /sys 0755 0 0
dir /run 0755 0 0
dir /etc 0755 0 0
dir /newroot 0755 0 0
file /init target/x86_64-unknown-linux-musl/release/ccrun-init 0755 0 0
```
The TLS root certificates are compiled into `/init` through `webpki-roots`. The workload's rootfs is a separate tmpfs mounted at `/newroot`. Do **not** mount it under `/run`: when §6.5 unmounts init's `/run`, it would be unmounted along with it.

**Kernel**: Linux 6.18.y LTS. Required features and the earliest version of each: tsm-mr sysfs (6.16), TDX halt fix (6.15), `cc_random_init` (6.9), CCEL sysfs (6.4). The quote length fix c3fd16c3b98e was backported to 6.18.y on 2026-04-02. build.sh must assert that the kernel version is no older than the first version that contains this fix. A **mandatory BadAML guard patch** is also applied: it forbids AML SystemMemory OperationRegions from mapping System RAM.

**Kconfig**: `make allnoconfig KCONFIG_ALLCONFIG=image/kernel.miniconfig`. Note that `merge_config.sh -s` only warns, and does not fail, when "requested value ≠ effective value". Codex pointed this out in its second round. So a separate assertion script of about 15 lines is needed. It compares every `=y` in the miniconfig, and every item on the must-be-off list, against the final `.config` one by one, and fails on any mismatch. Only the key points are listed below. The full miniconfig is settled by what M0 boots on C3 and N2D.
- **fragment (positive allowlist)**: `64BIT SMP MULTIUSER SHMEM POSIX_TIMERS PROC_SYSCTL FILE_LOCKING PRINTK HYPERVISOR_GUEST PARAVIRT X86_X2APIC ACPI EFI EFI_STUB PCI PCI_MSI VIRT_DRIVERS INTEL_TDX_GUEST TDX_GUEST_DRIVER AMD_MEM_ENCRYPT SEV_GUEST TSM_REPORTS CONFIGFS_FS NET INET UNIX PACKET NETDEVICES ETHERNET NET_VENDOR_GOOGLE GVE TCG_TPM TCG_TIS TCG_CRB TCG_SVSM SECURITYFS BLK_DEV_INITRD INITRAMFS_COMPRESSION_NONE TMPFS TMPFS_XATTR DEVTMPFS PROC_FS SYSFS SERIAL_8250 SERIAL_8250_CONSOLE TTY UNIX98_PTYS BINFMT_ELF BINFMT_SCRIPT FUTEX EPOLL NAMESPACES PID_NS EXPERT STATIC_USERMODEHELPER`, plus `STATIC_USERMODEHELPER_PATH=""`. This disables **all** usermode helpers, including core_pattern pipes and request_module.
- **must-be-off (asserted at build time)**: `MODULES KEXEC KEXEC_FILE DEVMEM MAGIC_SYSRQ COREDUMP USER_NS IO_URING BPF_SYSCALL USERFAULTFD VT INPUT_KEYBOARD SERIO DMI KVM_GUEST HW_RANDOM_TPM TCG_TPM2_HMAC LOAD_UEFI_KEYS ATA SCSI BLK_DEV_NVME USB_SUPPORT DRM SOUND VIRTIO HOTPLUG_PCI KPROBES HIBERNATION IA32_EMULATION DEBUG_FS XEN JAILHOUSE_GUEST ACRN_GUEST BHYVE_GUEST`. The last 4 still run hypervisor detection under `nopv` (`ignore_nopv`), so they must be turned off (verified against v6.18 `arch/x86/kernel/cpu/hypervisor.c:71`).
- `KVM_GUEST=n` is fine on TDX, because the TSC is forced to be treated as reliable. On N2D SNP, however, the clock source without kvmclock has **never been tested**. This is on the M0 list. If it causes problems, enable it for SNP only, at the cost of one more page shared with the host.

### 4.2 TDX (C3)

| Register | Contents (measured on GCE) | Verifier decision |
|---|---|---|
| MRTD | Google TDVF | Fetch the endorsement from `gce_tcb_integrity/ovmf_x64_csm/tdx/<MRTD>.binarypb` and first verify its RSA-PSS-SHA256 signature: `GCE-uefi-signer` → the pinned `GCE-cc-tcb-root_1`. **Pin the root, not the signer**. The signer rotates monthly. Once the signature verifies, MRTD must **appear in the signed contents, in `tdx.measurements[*].mrtd`**, and `svn ≥ floor`. On current firmware, MRTD does not depend on memory size |
| RTMR0 | `TdxTable` (TD HOB), `Fv(CFV)`, SecureBoot/PK/KEK/db/dbx, SEP, 3×`ACPI DATA`, BootOrder/Boot####, and Authority (when Secure Boot is on) | **Per-event syntax check** (§9.2). The TD HOB, the CFV and the ACPI triple must be in `platform.json`. All other events are only digest-bound (D9) |
| RTMR1 | `Calling EFI App`, SEP, `GPT`, **`BOOT_SERVICES_APPLICATION` = the PE's Authenticode SHA-384**, [optionally 1 `EBS Failed`], `EBS Invocation`, `EBS Success` | **Syntax + exact value**: GPT and App must equal `release.json`. No other application or driver event is allowed. `release.json` gives the RTMR1 value for both valid traces: the normal trace, and the trace with an extra `EBS Failed` when EDK2 retries ExitBootServices |
| RTMR2 | Expected to be all zeros: the stub's PCR9 events all go to the vTPM | Exactly 0 (**to be confirmed in M0**) |
| RTMR3 | ccrun ledger | Replay result must match exactly. Syntax in §5 |
| MROWNER | `SHA-384({"instanceId":…,"numericalProjectId":…,"zone":"…"})`, written by GCE | **Checked mandatorily by `ccrun up`** and written into the policy. `verify` also checks it when the policy contains this entry. It only protects against non-Google attackers, for example someone who uses the owner's published owner key to start a TD and hijack the launch |
| MRCONFIGID / MROWNERCONFIG / MRSERVICETD | 0 | Must be 0. If a v5 quote appears, MRSERVICETD must also be 0 |
| TD_ATTRIBUTES / XFAM | `0x10000000` / `0x602e7` | Pinned exactly |
| REPORTDATA | `SHA-512("ccrun/v1/evidence\0" ‖ nonce ‖ SHA-384(status))` | Binds both freshness and runtime status (§7.1) |

### 4.3 SNP (N2D Milan)

**Platform facts**: on GCE SNP, MEASUREMENT covers only Google OVMF and the VMSA. Measurements of 11 different images, 1127 observations in total, all gave the same value. So the PE's identity can only come from the vTPM's PCR4. Where the vTPM runs decides whether that identity can be trusted:

| | `snp-hostvtpm` (GA) | `snp-svsm` (alpha: `gcloud alpha … --svsm-config tpm=EPHEMERAL,snp-irq=UNRESTRICTED`; the image must declare `SNP_SVSM_CAPABLE`) |
|---|---|---|
| Where the vTPM runs | A software implementation on the host side (BoringSSL). The guest accesses it through TIS MMIO, and the VMM emulates those accesses through #VC/GHCB | Inside the SVSM (VMPL0, inside the TEE). The guest runs at VMPL2 |
| Root of trust for the PE identity | **The entire Google host**: the VMM can rewrite the firmware's PCR_Extend in transit, or impersonate the vTPM outright | AMD hardware + the Google-signed SVSM |
| What the SNP report proves | Only "this is a real SNP VM running Google firmware". Against a malicious VMM it **does nothing for code identity**: any kernel can get a genuine report and fill in arbitrary REPORT_DATA | The manifest v1 (including the AK) of the vTPM inside the SVSM is bound by hardware |
| verdict | `root=amd+google-host`. **FAIL unless the policy sets `allow_host_vtpm: true`**. `ccrun up --tee snp-hostvtpm` prints a prominent warning | `root=amd+google-svsm` |

**PCR rules shared by both modes**: SHA-256 bank. The quote covers PCR0–9. The firmware event log replay covers PCR0–7 and PCR9. PCR8 is replayed separately from the ledger.

| PCR | Contents | Rule |
|---|---|---|
| 0 | `S-CRTM_VERSION "GCE Virtual Firmware v2"`, `NONHOST_INFO` (tech=4 means SNP), SEP; plus SP800-155 `NO_ACTION` records (not extended) | Syntax check. Both S-CRTM and NonHostInfo require `H(data)==digest`, and the NonHostInfo value must be SNP |
| 1 | 3×`ACPI DATA`, BootOrder/Boot####, SEP | **The ACPI triple (SHA-256) must be in `platform.json[snp][machine-type]`**. Boot variables are only digest-bound (D9). In other words, **SNP also needs a reviewed baseline** |
| 2, 3, 6 | SEP only | **Exactly the "SEP only" constant** (`3d458cfe…`). Otherwise the host could load UEFI drivers through an NVRAM `Driver####` or a PCI option ROM. Such drivers are recorded only in PCR2, yet they can tamper with the kernel after the Authenticode measurement |
| 4 | `Calling EFI App`, SEP, `BOOT_SERVICES_APPLICATION` (the PE's Authenticode SHA-256) | Exactly equal to `release.json` |
| 5 | [SEP], GPT, [EBS Failed], EBS Invocation, EBS Success | GPT equals the release. Whether SEP appears, and where, is settled by M0 measurements and then fixed into the two traces in release.json |
| 7 | SecureBoot/PK/KEK/db/dbx, SEP, and Authority (when Secure Boot is on) | Only digest-bound (D9) |
| 9 | The stub's `EV_EVENT_TAG` (LoadOptions) | **Not appraised**. At most 1 event is allowed. The tag label is not covered by the digest, so it proves nothing. What actually stops initrd injection is the `noinitrd` built into the pinned PE (M0 includes an adversarial test) |
| 8 | ccrun ledger | Replayed from the ledger; must match exactly |

**Report and freshness**: once an SNP guest request fails, the kernel zeroes the VMPCK, and no further report can be obtained. So **the report is fetched only once per boot**, after ledger event 0 and before HTTP is opened. If the fetch fails, the VM shuts down. There is no retry.
- `snp-hostvtpm`: `REPORT_DATA = SHA-512("ccrun/v1/snp-boot\0" ‖ boot_id ‖ SHA-256(AK TPMT_PUBLIC))`.
- `snp-svsm`: **service attestation** through configfs-tsm:
  - `service_provider=svsm`;
  - `service_guid=c476f1eb-0123-45a5-9641-b4e7dde5bfe3`;
  - **`service_manifest_version=1`** (the default, version 0, gives a manifest that contains only the EK and cannot bind the AK);
  - `inblob = SHA-512("ccrun/v1/snp-boot\0" ‖ boot_id)`;
  - this yields `REPORT_DATA = SHA-512(inblob ‖ manifest)`.
- Freshness: each request gets a new **TPM quote**, with `qualifyingData = SHA-256("ccrun/v1/quote\0" ‖ nonce ‖ SHA-256(report) ‖ SHA-384(status))`. The `boot_id` the verifier uses comes from `ledger[0]`, not from a JSON field.

**Guest backend**: only configfs-tsm file I/O and hand-written TPM2 commands. There is no dependency on `tss-esapi` or libtss2, and none on the `sev` crate.
1. Fetch the report: use `privlevel_floor` rather than hard-coding VMPL0. Keep `outblob`. `auxblob` is for debugging only.
2. TPM (`/dev/tpmrm0`):
   - `NV_ReadPublic` and `NV_Read` (owner auth, empty password, chunked reads) read the AK template (`0x01c10001`). **Both modes read it**, because the SVSM also writes the AK template into a Google NV index. The exact index is to be confirmed in M0. Only `snp-hostvtpm` also reads the AK certificate (`0x01c10000`). `snp-svsm` has no certificate that carries the instance identity, so in this mode the instance binding in the policy is empty.
   - `CreatePrimary` (Endorsement hierarchy). `snp-hostvtpm` requires the modulus of the generated key to equal the public key in the certificate. `snp-svsm` requires TPMT_PUBLIC to equal one of the entries in manifest v1 and to carry the `restricted|sign|fixedTPM` attributes.
   - `PCR_Extend(8)`, `Quote` (NULL scheme), `FlushContext`.
   A 132-line, std-only draft was written during the research phase. It compiles, but **has never run against a real TPM**.
3. Event log: mount securityfs, read `/sys/kernel/security/tpm0/binary_bios_measurements`, and parse it with the same TCG2 parser that CCEL uses.

### 4.4 Pre-pull input inventory (TDX as the example; SNP is the same with RTMR replaced by PCR)

| Input | Source | Handling |
|---|---|---|
| Firmware code | Google | MRTD is in the signed endorsement contents |
| PE (kernel, initramfs, cmdline, `/init`) | ccrun release | RTMR1 exact (Authenticode) |
| GPT | Disk (provided by the host) | RTMR1 exact; the guest never writes to disk |
| ACPI tables (AML is code) | Host | RTMR0 must be in the reviewed baseline; the BadAML patch limits AML's access to RAM |
| TD HOB, CFV | Host, firmware | RTMR0 must be in the baseline (v2 instead derives them offline from the endorsed `.fd`) |
| NVRAM: Boot####, BootNext, Driver####, Secure Boot variables, LoadOptions | Host | **Harmless by construction**: RTMR1 pins the PE that is actually loaded, and no driver events are allowed; the PE ignores LoadOptions and loads no initrd; the kernel does not read UEFI keys |
| CPUID (including 0x4000_00xx) | TDX module and host | The security-relevant parts are covered by XFAM and the attributes; PV leaves are masked by `nopv` |
| SMBIOS/DMI | Host (not measured) | `DMI=n`; the kernel does not parse it |
| Serial input, PS/2 keyboard | Host | `MAGIC_SYSRQ=n VT=n INPUT_KEYBOARD=n`; the workload's stdin is connected to `/dev/null`. **In v0.1 this was a real vulnerability**: the host could use SysRq to make the kernel dump GPRs to the serial port |
| Device MMIO/PCI (gVNIC, vTPM TIS, UART, RTC) | Host | **Attack surface**: parsed as untrusted data by measured drivers; the driver set is cut to the minimum (D10) |
| DHCP replies | Host | Handled by a bounded parser in `/init`; affects only network availability; the hostname is not taken from DHCP |
| metadata (`ccrun-owner`) | Host | Measured first as ledger event 1, then used |
| RTC time | Host | Used only to check TLS validity periods, with the owner-signed `issued_at` as a lower bound (§6.3) |
| Randomness | RDRAND/RDSEED | Trusted; `HW_RANDOM_TPM=n` |
| launch config | owner | Ledger event 2, signed |
| Secrets | owner | HPKE-encrypted; only the hash of the ciphertext is recorded in ledger event 2 (for later audit) |
| CCEL and event log | Firmware memory | Not trusted in itself; authenticated by replay and by digest binding per event type |

---

## 5. Runtime ledger

The encoding follows dstack v1, so existing tools can replay it directly:
- `preimage = u32le(0x08000001) ‖ ":" ‖ name ‖ ":" ‖ payload`
- TDX: `RTMR3 ← extend(SHA-384(preimage))`, that is, write 48 bytes to `…/tdx_guest/measurements/rtmr3:sha384`.
- SNP: `PCR8 ← extend(SHA-256(preimage))`.

| # | name | payload | When |
|---|---|---|---|
| 0 | `ccrun.boot` | `ver ‖ boot_id(16) ‖ hpke_pk(32)` | After boot, before any network I/O |
| 1 | `ccrun.owner` | The **raw bytes** of `ccrun-owner` in the metadata | Recorded as soon as it is read (measure first, parse later) |
| 2 | `ccrun.launch` | `raw config bytes ‖ SHA-512(enc‖ct)` | After the signature and HPKE checks pass and the CAS is won, **before any registry request is sent** |
| 3 | `ccrun.exec` | The resolved platform manifest digest | After unpacking, before execve (closes the ledger) |

Rules:
- The state and the ledger prefix must correspond:

| State | Ledger |
|---|---|
| WAITING | `[0,1]` |
| STARTING, PULLING | `[0,1,2]` |
| RUNNING, EXITED | `[0,1,2,3]` |
| FAILED | `[0,1,2]` or `[0,1,2,3]` |

- **Consistent snapshot**: extend operations, state changes and evidence generation all hold the same lock. The evidence handler reads the status and the ledger inside the lock, gets the quote, then releases the lock. So the status, the ledger and the quote always describe the same moment.
- The verifier replays from all zeros, and the result must match exactly. So init no longer checks the initial register values itself (v0.2 removed this step, which also drops the TPM PCR_Read).
- The ledger is append-only and must match exactly, so **nobody can make the verifier accept a different config**. At most they can make it FAIL.

---

## 6. `ccrun-init`

### 6.1 State machine
```
BOOT ─► WAITING ─(launch, CAS)─► STARTING ─(commit)─► PULLING ─► RUNNING ─► EXITED
  │                         └──────── any failure ──► FAILED (terminal; still serves evidence, no retry)
  └─ failure ─► print "ccrun: FAIL <step>: <err>" to serial ─► power off
```
`status` (the canonical JSON of `{"state","exit_code","error"}`) is bound into REPORTDATA (TDX) or qualifyingData (SNP).

### 6.2 BOOT
1. Mount proc, sys, devtmpfs, `/run` (tmpfs) and configfs. On SNP, also mount securityfs. Set `PR_SET_DUMPABLE=0`. PID1's fds 0, 1 and 2 are pointed at `/dev/null`. `/dev/console` is opened only briefly, when something must be printed.
2. Determine the TEE type from the configfs-tsm `provider` (`tdx_guest` or `sev_guest`). Generate `boot_id` and an HPKE X25519 key pair, and extend event 0. On SNP, the one and only report is taken at this step.
3. Network:
   - use netlink to set `lo` UP;
   - run DHCP (`dhcproto`, UDP + `SO_BINDTODEVICE`, **with the BROADCAST flag set**), and configure the address, the MTU (option 26) and the option 121 routes;
   - write PID1's own `/etc/resolv.conf` (from option 6);
   - the hostname is fixed to `ccrun`.
   v1 does not do T1 lease renewal. If M0 finds that GCE reclaims the address after the lease expires, it will be added.
4. Read `http://169.254.169.254/computeMetadata/v1/instance/attributes/ccrun-owner` (with the `Metadata-Flavor: Google` header). Extend event 1 with the raw bytes first, then parse them.
5. Start an HTTP server on `:7700` (`std::net` + `httparse`): one thread per connection, with a read timeout and a body size limit. `/v1/evidence` has a concurrency of 1 and is rate limited. Enter WAITING.

### 6.3 STARTING and PULLING
1. On `POST /v1/launch`, first run the checks that have **no side effects**: verify the signature and open the HPKE seal. Then CAS WAITING→STARTING under a mutex. Only the winner continues. All other requests get 409.
2. **Time floor**: if the RTC is earlier than `issued_at` (which the owner signature covers), use `clock_settime` to set the system time to `issued_at`. From then on, every TLS certificate check sees a time ≥ `issued_at`, so the host cannot roll the clock back into the validity period of some expired certificate.
3. Extend event 2, enter STARTING, and wait for the commit.
4. On `POST /v1/commit`: `sig = Ed25519(owner_sk, SHA-512("ccrun/v1/commit\0" ‖ boot_id ‖ SHA-384(ledger[0..=2])))`. Once the signature verifies, enter PULLING. Before sending the commit, the CLI has already verified the STARTING state against fresh evidence.
5. **Layer-by-layer pipeline**: download → compute sha256 while downloading, and abort once `descriptor.size + 1` bytes have been read → verify → hand to the unpack helper → delete the blob. Peak memory is the rootfs plus the largest layer.
6. Credentials:
   - accept only **short-lived tokens** delivered by the owner (such as the output of `gcloud auth print-access-token`);
   - send them only to authorized **full HTTPS origins** (scheme, host and port must all match): the origin of the registry that hosts `image`, and the token realm origins listed explicitly in the config (`registry_auth.realm_origins`);
   - do not send Authorization on redirects (the ureq default).
   - If `vm-service-account` is chosen, `ccrun up` must explicitly name a service account that has only Artifact Registry read access. All other modes always use `--no-service-account --no-scopes`.

### 6.4 Unpack helper (`/init --stage unpack`, single-threaded, chroots to `/newroot` first)
- Inside the chroot, absolute symlinks resolve only within the rootfs, for example `/var/run -> /run`.
- **Two passes per layer**: the first pass handles only `.wh.*` and `.wh..wh..opq`, and applies only to the lower layers. The second pass handles all other entries.
- **Replace semantics**: `lstat` each entry first. Unless both the old and the new entry are directories, first `remove_dir_all` or `remove_file`, then `unpack_in`. This correctly handles cases such as a hard link being republished or a directory being replaced by a file.
- Skip char and block device nodes. Preserve FIFOs, uid, gid, mode and xattrs (including `security.capability`).
- gzip uses `flate2::MultiGzDecoder` (eStargz is multi-member gzip), and zstd uses `ruzstd`. After the tar has been read, the decompressor must be **read to EOF**, and the compressed blob must be fully consumed. Any leftover data is a failure. Otherwise Docker and ccrun could see different rootfs contents.
- Write `/etc/resolv.conf` and `/etc/hosts` (with `127.0.0.1 localhost` and `::1 localhost`). Secrets are written to `/run/secrets/<name>`: first chown them to the workload's uid, then set them to 0400.
- When it finishes, PID1 extends event 3.

### 6.5 Run helper (`/init --stage run`, single-threaded) and D7
1. The helper calls `unshare(CLONE_NEWNS|CLONE_NEWPID)`, then forks. The child becomes process 1 in the new pid ns.
2. While still privileged, the child does the following, in order:
   1. `mount --make-rprivate /`;
   2. mount a new proc in the rootfs: remount `/proc/sys`, `/proc/sysrq-trigger`, `/proc/irq`, `/proc/bus` and `/proc/fs` read-only, and mask `/proc/kcore`, `/proc/keys` and `/proc/timer_list` with empty files (the same as Docker's readonly/masked paths). Also mount a read-only sysfs and a `/dev` tmpfs (containing only null/zero/full/random/urandom/tty, plus devpts and shm);
   3. **run `umount2(MNT_DETACH)` on init's `/proc`, `/sys` (along with configfs and securityfs), `/dev` and `/run`**;
   4. `chdir("/newroot")`, `MS_MOVE` it to `/`, then `chroot(".")` (the switch_root approach; with kernel ≥7.0, pivot_root can be used instead). The mount points from step 2 are all under `/newroot/...`, and step 3 unmounts init's own mount points, so the two do not interfere;
   5. capabilities: the bounding set = Docker's default set minus `MKNOD` and `SYS_CHROOT`; clear the inheritable and ambient sets;
   6. setgroups, setgid, setuid, then chdir;
   7. `close_range(3, ~0, CLOSE_RANGE_CLOEXEC)`: this only **marks** the remaining fds CLOEXEC and does not close them, so the handshake pipe survives until execve. stdin is connected to `/dev/null`. stdout and stderr go to the serial port or to `/dev/null`, according to `config.logs`;
   8. **exec handshake**: if any of steps 1 to 7 fails, the child writes `(step, errno)` to the handshake pipe and then calls `_exit`. After these steps, it calls execve.
3. The helper's decision:
   - It reads `(step, errno)`: FAILED.
   - It reads EOF: execve has passed the point of no return, and the state becomes RUNNING. Note that the workload may still exit right after this, so **`running` in the status only means "execve was committed, and the process was still alive at the moment the quote was taken"**. The evidence handler confirms this with `waitpid(WNOHANG)` inside the consistency lock.
   - The helper reaps the workload and passes its exit code to PID1.

End result: the workload cannot see configfs, a writable sysfs, TEE or TPM devices, `/proc/1` or `core_pattern`. The kernel also has no usermode helper, SysRq, user ns, io_uring or BPF. The workload can still reach `localhost:7700`, which offers only the read-only evidence interface and a launch interface that always returns 409 after launch. The request parsing of these two interfaces is attack surface, so it is kept minimal.

---

## 7. Protocol

The protocol runs over plaintext HTTP. Security comes from signatures, HPKE, the quote and the status binding.

### 7.1 `GET /v1/evidence?nonce=<64 hex>`
```json
{ "v": 1, "tee": "tdx|snp-svsm|snp-hostvtpm",
  "status": "<b64 canonical JSON: {state, exit_code, error}>",
  "ledger": [{"name": "...", "payload": "<b64>"}],
  "tdx": { "quote": "<b64>", "ccel": "<b64, 0xFF padding removed>" },
  "snp": { "report": "<b64>", "manifest": "<b64>|null", "tpm_quote": {"attest": "<b64>", "sig": "<b64>"},
           "ak_public": "<b64>", "ak_cert": "<b64>|null", "eventlog": "<b64>" } }
```
- TDX: `REPORTDATA = SHA-512("ccrun/v1/evidence\0" ‖ nonce ‖ SHA-384(status))`, with one quote per request. See 4.3 for how SNP does the binding.
- Optional parameter `&acpi=1`: the response also carries the raw contents of `/sys/firmware/acpi/tables/*`, for review by `golden` only. Binding: first build `acpi_manifest`, a list of `(name, SHA-384(content))` sorted by file name. TDX uses `REPORTDATA = SHA-512("ccrun/v1/evidence\0" ‖ nonce ‖ SHA-384(status) ‖ SHA-384(acpi_manifest))`. SNP appends `SHA-384(acpi_manifest)` to the TPM quote's qualifyingData. This is only **best-effort evidence**: the tables are read after AML has executed, and they have **no cryptographic link** to the pre-patch digest in RTMR0 (see §13.1).

### 7.2 `POST /v1/launch`
```json
{ "boot_id": "...", "issued_at": 1792000000, "config": "<b64>",
  "sealed": {"enc": "<b64>", "ct": "<b64>"} | null, "sig": "<b64>" }
```
- `sig = Ed25519(owner_sk, SHA-512("ccrun/v1/launch\0" ‖ boot_id ‖ hpke_pk ‖ u64(issued_at) ‖ SHA-512(config) ‖ SHA-512(enc‖ct)))`
- HPKE uses RFC 9180 base mode with the suite X25519 / HKDF-SHA256 / ChaCha20-Poly1305, and `info = "ccrun/v1/secrets\0" ‖ boot_id`.
- **config is public**: it travels in plaintext on the wire, and ledger event 2 later returns it to anyone who can reach `:7700`. So env may hold only non-secret configuration. Every secret goes through `--secret`.

### 7.3 `POST /v1/commit`
```json
{ "boot_id": "...", "ledger_sha384": "<hex>", "sig": "<b64>" }
```
Accepted only in the STARTING state, and only once.

### 7.4 launch config (the copy that is measured)
```json
{ "image": "us-docker.pkg.dev/p/r/agent@sha256:<64hex>",
  "entrypoint": null, "cmd": null, "env": ["MODEL=..."], "workdir": null, "user": null,
  "secrets": ["OPENAI_API_KEY"],
  "registry_auth": {"mode": "none|sealed-token|vm-service-account", "realm_origins": ["https://us-docker.pkg.dev"]},
  "logs": "serial|none",
  "tls_spki_sha256": null }
```
- Parsing uses `deny_unknown_fields`, and **only image references with a digest are accepted**.
- `tls_spki_sha256` is for application channel binding. The owner delivers the workload's TLS private key with `--secret` and writes the SPKI hash of the matching public key into the config, so the hash is measured. `ccrun verify` prints it, and clients use it to pin the workload's certificate.
- Why it is needed: without it, the host could forward the real evidence on `:7700` while swapping `:8080` for a different service.

---

## 8. Container semantics (differences from `docker run`)

Supported:
- OCI and Docker manifests and indexes (linux/amd64 is selected);
- layer formats: tar, gzip (multi-member supported), zstd (single frame only; multi-frame is rejected);
- entrypoint, cmd, env, workdir, user;
- both kinds of whiteout, replacement semantics, file capabilities.

Differences from `docker run`:
- networking is equivalent to `--net=host`, and `lo` is available;
- no cgroups and no seccomp;
- the capability set lacks `MKNOD` and `SYS_CHROOT`;
- no user namespaces, so programs such as bubblewrap and the Chrome sandbox fail;
- no io_uring;
- stdin is `/dev/null`;
- only one container can run, and its rootfs is in RAM.

---

## 9. Verifier

### 9.1 Inputs
- **`release.json`**: generated by `ccrun measure`. It contains the PE's Authenticode SHA-384 and SHA-256, the GPT event digests (both banks), and the expected RTMR1/RTMR2 and PCR4, PCR5, PCR2/3/6.
- **`platform.json`**: a baseline that the maintainers review and sign. It contains:
  - `tdx[MRTD] = {cfv, td_hob[machine-type]}`;
  - `acpi[tdx|snp][machine-type] = [{digests, iasl_dump_sha256, reviewed_by, date}]`;
  - TCB minimums.

  Users can also generate a proposal with `ccrun golden`, review it, and sign it with their own key.
- **`NAME.policy.json`**: written by `ccrun up`. It is **self-contained**: a third party needs nothing else. It contains:
  - `tee` (**required**; the evidence's mode must match it, which prevents downgrade);
  - `machine_type`;
  - the owner public key and the raw config bytes;
  - **release identity**: the PE and GPT digests copied from release.json (both banks), both RTMR1 traces, the expected values of PCR2/3/6/4/5, and the release version number;
  - instance binding (PZID for TDX; the identity in the AK certificate for `snp-hostvtpm`; empty for `snp-svsm`);
  - the relevant baseline entries copied from platform.json;
  - TCB minimums, `allow_host_vtpm`, and the optional `tls_spki_sha256`.

### 9.2 TDX
1. Verify the quote with `dcap-qvl =0.7.0`: `default-features=false`, with `[std, rustcrypto, default-x509]` enabled (101 crates in total), running offline. `danger-allow-tcb-override` is not enabled. `verify()` only does the cryptographic checks and computes the TCB status. **We must apply the policy ourselves** (Codex's second round pointed out that v0.2 missed this step):
   - collateral is the latest standard version from Intel PCS v4. Require `tcbEvaluationDataNumber ≥ min_tcb_eval_num`, and check `nextUpdate > now`. `now` is the verifier's local clock, which is the time the relying party itself trusts;
   - the platform, TDX module and QE statuses must all be in `allowed_status`, which by default holds only `UpToDate`;
   - advisories must be ⊆ `allowed_advisories`;
   - after Intel publishes a TCB recovery and before Google finishes upgrading, the policy can set `tcb_grace_until` with a deadline. Until that deadline, OutOfDate is only a warning. The v0.2 approach of "fetching collateral for the old eval version" is dropped: PCS eventually takes old versions down with HTTP 410.
2. Check REPORTDATA (nonce + status), attributes, XFAM, MRCONFIGID/MROWNERCONFIG/MRSERVICETD, and MROWNER == PZID.
3. MRTD must appear in the signed content of the endorsement (§4.2).
4. **Parse the CCEL**: bounded parsing, SHA-384 bank only, and `0xFF` is allowed only as trailing padding. `EV_NO_ACTION` records with index 0 (the Spec ID header, SP800-155 Event3) are handled separately: their length and count are limited, and they are not replayed. All other events may only use MR index 1..3. The replay result must equal RTMR0..2.
5. **Bind digests by event type**:
   - `H(data)==digest`: SEPARATOR, EFI_ACTION, GPT, VAR_DRIVER_CONFIG, VAR_AUTHORITY; for SNP also S_CRTM_VERSION, NONHOST_INFO;
   - `H(VariableData)==digest`: VAR_BOOT (a GCE quirk);
   - opaque comparison, event data ignored: HANDOFF_TABLES2 (TD HOB), FIRMWARE_BLOB2 (CFV), ACPI DATA (PLATFORM_CONFIG_FLAGS), BOOT_SERVICES_APPLICATION; for SNP also the EVENT_TAG in PCR9 (not evaluated, at most 1).
   Then check the log against the grammar in §4.2. Any unlisted event type or extra event is a FAIL.
6. RTMR2 == 0. RTMR3 == the ledger replay, and the ledger follows the grammar in §5. `ledger[1]` == the owner public key in the policy. If the ledger contains #2, the config in it must equal the config in the policy. The ledger prefix must match the state in the status (§5).
7. Output PASS, listing each claim, `root=intel`, and the list of remaining assumptions (the Google firmware binaries, the reviewer and date of the ACPI baseline, and so on).

### 9.3 SNP
1. **report**: use `sev =8.0.0` (`default-features=false`, features `[snp, crypto_nossl]`, pure RustCrypto). A prototype of about 52 lines has verified the signature chain of a real GCE report, and it also rejects a tampered report. We must add the checks the crate does not do:
   - The version must be in the set that sev 8 supports. An unknown version is a FAIL. `SIGNATURE_ALGO==1`.
   - Fetch the VCEK from AMD KDS (by CHIP_ID and REPORTED_TCB, with caching). Verify the chain VCEK ← ASK ← ARK (Milan, with the DER hash pinned) and the certificate validity periods.
   - The VCEK's hwID == CHIP_ID. The VCEK's SPL extensions == REPORTED_TCB. Any missing extension is a FAIL.
   - signing_key is VCEK, and `MASK_CHIP_KEY==0`.
   - DEBUG and MIGRATE_MA are both 0, checked bit by bit. **Do not** require the policy to equal the value in the endorsement.
   - All four TCB components are ≥ the minimums.
   - FAMILY/IMAGE/ID_KEY/AUTHOR_KEY/HOST_DATA are all 0.
   - VMPL: `snp-hostvtpm` requires 0. `snp-svsm` requires 0, and MEASUREMENT must equal `svsm_measurement`.
2. **Endorsement** (fetched from GCS and cached; the signature chain is shared with TDX):
   - `snp-hostvtpm`: MEASUREMENT may be any entry in `sev_snp.measurements` (each entry is firmware that Google has signed), but it must **not** be `svsm_measurement`;
   - `snp-svsm`: MEASUREMENT must equal `svsm_measurement`.
3. **REPORT_DATA**: recompute it with `ledger[0].boot_id`. For `snp-hostvtpm`, add the AK. For `snp-svsm`, add the inblob and manifest v1.
4. **AK**:
   - `snp-hostvtpm`: verify the Google AK certificate chain, which ends at the pinned `EK/AK CA Root`. The intermediate certificate is fetched via AIA and its signature is verified, but it is not pinned (it was just rotated in 2026-05). isProduction must be true, and the instance identity must match the policy.
   - `snp-svsm`: the full TPMT_PUBLIC of the key used for the quote must equal an entry in manifest v1, and it must have `restricted|sign`.
5. **TPM quote**: first confirm that TPMS_ATTEST has `magic==0xff544347` and `type==TPM_ST_ATTEST_QUOTE`, and that the PCR selection is exactly PCR0–9 of the SHA-256 bank. Then verify the signature, confirm that `extraData` covers the nonce, the report and the status (and the optional ACPI manifest), and confirm that pcrDigest matches the PCR values.
6. **PCR**: the firmware log replay equals PCR0–7 and PCR9. Then apply the rules for each PCR from the table in 4.3. The rules for binding digests by event type are the same as in step 5 of 9.2, but on the SHA-256 bank. PCR8 equals the result of the ledger replay.
7. In `snp-hostvtpm` mode, if the policy does not have `allow_host_vtpm`, **the result is FAIL**.

---

## 10. Build and release
1. The build container pins every version: Rust (target `x86_64-unknown-linux-musl`), musl-gcc (needed by `ring`), the kernel toolchain, mtools and gdisk.
2. Build `/init`: `cargo build --locked --release`, with `--remap-path-prefix` and `codegen-units=1`. `SOURCE_DATE_EPOCH` is the commit time.
3. Build the kernel:
   - use 6.18.y source pinned by sha256, with the BadAML patch applied;
   - run `make allnoconfig KCONFIG_ALLCONFIG=kernel.miniconfig`;
   - run the assertion script: every `=y` in the miniconfig, and every item in must-be-off, must match the final `.config`;
   - set `KBUILD_BUILD_{TIMESTAMP,USER,HOST,VERSION}` and build `bzImage`, which is `BOOTX64.EFI`.
4. Make the disk (following the Flashbots approach):
   - a 1 GiB `disk.raw`;
   - `sgdisk` writes a fixed disk GUID and ESP GUID, and the ESP starts at LBA 2048;
   - format with `mkfs.vfat --invariant` and write the PE with `mcopy`;
   - pack with `tar --format=oldgnu -Sczf`.
5. Generate `ccrun measure disk.raw > release.json`. **The reproducibility promise covers only release.json**, that is, the GPT and the Authenticode hash. Reproducible FAT filesystem bytes are a bonus. CI builds independently on two machines, and release.json must match byte for byte.
6. `ccrun image publish`: first `gcloud storage cp`, then `gcloud compute images create --guest-os-features UEFI_COMPATIBLE,GVNIC,VIRTIO_SCSI_MULTIQUEUE,TDX_CAPABLE,SEV_SNP_CAPABLE[,SNP_SVSM_CAPABLE]`. `SEV_CAPABLE` is not included: plain SEV has no attestation, so the image would only shut down if booted there.

---

## 11. Usage

```bash
ccrun keygen                                    # generates ~/.config/ccrun/owner.ed25519
ccrun image publish --project P --bucket B      # or use the official public image directly
ccrun up agent1 --tee tdx --zone us-central1-a --machine-type c3-standard-4 \
  --image us-docker.pkg.dev/p/r/agent@sha256:… -p 8080 \
  -e MODEL=x --secret OPENAI_API_KEY=@./key.txt
ccrun verify http://IP:7700 --policy agent1.policy.json
ccrun logs agent1        # view the serial output
ccrun down agent1        # delete the instance and its dedicated firewall rule
```

`ccrun up` is **idempotent**. It acts on the instance's current state:
- **Instance does not exist**: first run preflight (gcloud auth, project, whether the zone supports the TEE, whether an organization policy forbids external IPs). Then create it with **fixed parameters that were tested on real hardware**:
  - `--confidential-compute-type TDX|SEV_SNP`; for SNP add `--min-cpu-platform "AMD Milan"`;
  - `--maintenance-policy TERMINATE`;
  - `--network-interface nic-type=GVNIC`;
  - `--boot-disk-type pd-balanced`, with only this one disk attached; `-lssd` machine types and extra disks are rejected;
  - `--no-shielded-secure-boot --shielded-vtpm`;
  - `--no-service-account --no-scopes` (except in `vm-service-account` mode);
  - `--metadata ccrun-owner=<pub>`;
  - `--tags ccrun-<NAME>`, plus a firewall rule created just for this instance that allows only 7700 and the ports given with `-p`.
- **Instance is stopped**: start it.
- **In WAITING**: re-verify the pre-pull state, then deliver. A VM that restarts automatically after host maintenance returns to WAITING, and this command recovers it.
- **In STARTING** (the previous launch response was lost, or the commit was never sent): verify the STARTING state and confirm that `ledger[2]` is exactly the config in this request, then resend the commit. If the config differs, report an error and suggest `--recreate`.
- **In PULLING**: wait, then verify.
- **In RUNNING**: verify, and confirm that the config matches.
- **In FAILED or EXITED**: these two are terminal states, and the same boot cannot launch again. `ccrun up --recreate` runs `gcloud compute instances reset`, which gives a fresh boot back in WAITING, and then follows the normal flow (secrets must be delivered again; the CLI has them all locally).
- While polling for evidence, it also polls the instance state. If the instance enters TERMINATED or the wait times out, it prints the tail of the serial output (which contains `ccrun: FAIL <step>`).
- If the ACPI or TD HOB is not in the baseline: **FAIL immediately** and suggest running `ccrun golden --propose`. **There is no interactive TOFU.**

The flow of `ccrun golden --propose --tee tdx --machine-type c3-standard-4`:
1. Start a VM and let it stop in WAITING;
2. Fetch evidence with `?acpi=1`; verify everything except ACPI, TD HOB and CFV as usual;
3. Generate a proposal: the digest, the `iasl -d` disassembly, the diff against the previous baseline, and automatic checks (whether `Load`/`LoadTable` appears, whether any `SystemMemory` region points at RAM);
4. Delete the VM.

A proposal must be reviewed and signed by a person before it can enter platform.json.

---

## 12. Code size and dependency budget (re-estimated for v0.3)

| Component | Lines | Dependencies |
|---|---|---|
| init: boot, mounts, state machine, status | 180 | `rustix` |
| init: network (netlink setup of lo/eth0, DHCP, resolv.conf) | 200 | `dhcproto` |
| init: HTTP server | 100 | `httparse` |
| init: TEE backends (configfs-tsm, RTMR3, CCEL; the SNP report and hand-written TPM2 code) | 120 + 180 | none |
| init: launch and commit (signature checks, HPKE, CAS, time floor, consistency lock) | 130 | `ed25519-dalek`, `hpke`, `serde_json` |
| init: registry client (pull by digest only, realm allowlist) | 200 | `ureq 3` (`rustls` + `ring` + `webpki-roots`) |
| init: unpack helper (two passes, replacement semantics, multi-member gzip, read to EOF) | 200 | `tar`, `flate2`, `ruzstd` |
| init: start helper (ns, unmounting mounts, MS_MOVE+chroot, capabilities, exec handshake, reap) | 240 | `rustix` |
| core: schema, ledger, TCG2 parsing, binding by event type | 300 | `sha2`, `serde` |
| cli: gcloud wrapper (the branches of up, down/logs/golden/preflight/firewall) | 500 | — |
| cli: fetching and caching collateral and endorsements (PCS, KDS, AIA, GCS) | 180 | `ureq` |
| cli: endorsement verification (shared by TDX and SNP) | 80 | `prost` (derive), `rsa` |
| cli: TDX verifier (including TCB policy) | 340 | `dcap-qvl` |
| cli: SNP verifier (extra report checks, TPM quote, AK chain, PCR rules) | 450 | `sev`, `x509-cert`, `rsa` |
| cli: measure | 80 | `authenticode`, `object` |
| scripts, kernel miniconfig, must-be-off list, assertion script | about 300 | — |
| **Total** | **about 3.6k lines of Rust + about 300 lines of scripts and config + 1 kernel patch of about 150 lines** | |

Notes:
- The verifier is not part of the guest's TCB, but it **is part of the relying party's TCB**, so it is security-critical code too.
- Pulling in the `sev` crate makes the verifier link two generations of RustCrypto at once. This is a trade-off made to reduce code size.
- If M0 shows that `snp-svsm` is unusable, and the owner decides not to ship `snp-hostvtpm`, about 650 lines of code and the `sev` crate can be cut, and the kernel no longer needs the TPM driver.

---

## 13. Remaining risks (an honest statement)
1. **Host ACPI**: this is the only code that the host supplies and the kernel executes. Baseline review has limited power (iasl disassembly plus human reading), and drift is frequent: in 2026 at least 5 distinct triples appeared on c3-standard-4, and within a single run 1 of 3 VMs differed. The only runtime mitigation is the BadAML patch. It only blocks SystemMemory regions that access RAM, and it is **not** a full AML sandbox. Moreover, **there is no cryptographic link between the table contents the reviewer sees (post-patch, read out after AML has executed) and the digests in RTMR0 (of the pre-patch fw_cfg blobs)**: the reviewer can only trust that "the same VM produced both these tables and these digests". A real link requires reconstructing the pre-patch blobs from the post-patch tables (v2). So the precise statement of "meets requirement 3" in `tdx` mode is: **it is met, provided the reviewed ACPI digests are trusted**. **This is the largest residual risk of the TDX design, and also its largest operational burden.**
2. **Google firmware and SVSM are closed source**: the only option is to rely on Google's signatures.
3. **Device driver attack surface**: gVNIC, tpm_tis, 8250 and rtc_cmos all parse host-supplied data, and upstream TDX/SNP guests have neither device filtering nor CoCo hardening for these drivers.
4. **Unmeasured owner input**: the plaintext of the secrets is not measured. The ledger holds only the hash of the ciphertext, and the owner can later open the plaintext to an auditor for checking.
5. **Public information**: config and env are public; the serial log (with `logs=serial`) is visible to the host.
6. **Credential confidentiality**: this relies on WebPKI plus the `issued_at` time floor, and certificate revocation is not checked.
7. **Availability**: after host maintenance the owner must run `ccrun up` again. Changes to ACPI, firmware or TCB can cause false FAIL results, which require a baseline update.
8. **`snp-hostvtpm`**: does not meet requirement 3 against a malicious host.
9. **The workload itself**: ccrun only proves "this is what is running". Once the workload is compromised, it can leak the secrets it holds.

---

## 14. M0: what must be confirmed on real hardware before building (about 2 days)
1. **TDX (c3-standard-4)**:
   - a plain EFI stub PE (allnoconfig kernel) boots as `BOOTX64.EFI`;
   - RTMR1 has exactly 6 events (the EBS retry trace can only be derived from source and is hard to trigger on real hardware, so release.json gives both traces), and RTMR2 is 0;
   - with `--no-shielded-secure-boot`, the RTMR0 event grammar matches §4.2;
   - after RTMR3 is written, replay gives a matching result.
2. **Adversarial boot tests**: inject each of the following one at a time, and confirm that either the PE is unaffected or the verifier FAILs:
   - `BootNext` plus `initrd=\x` set in NVRAM;
   - an initrd supplied through LoadFile2;
   - `Driver####`;
   - malformed LoadOptions;
   - tampered Boot variables.
3. **Network**:
   - whether GCE's DHCP replies by unicast (this decides whether the BROADCAST flag is needed);
   - whether option 121, lo and resolv.conf work correctly;
   - whether metadata and external registries are reachable;
   - whether the IP is reclaimed after the lease expires.
4. **Image**: whether one image that declares both `TDX_CAPABLE` and `SEV_SNP_CAPABLE` boots on both C3 and N2D; whether N2D uses gVNIC.
5. **N2D SNP**:
   - the clock source with `KVM_GUEST=n`;
   - whether PCR2/3/6 contain only SEP; the exact event grammar of PCR5;
   - whether PCR4 has only one application event;
   - whether the vTPM uses TIS;
   - what the auxblob contains.
6. **`snp-svsm`** (decides whether SNP can ship):
   - whether ordinary projects can use `--svsm-config`;
   - whether the `tpm_svsm` driver is available, and whether the guest runs at VMPL2;
   - **whether manifest v1 (with the AK) can be obtained**; which NV index holds the AK template;
   - whether OVMF measures `BOOTX64.EFI` into PCR4 of the SVSM vTPM;
   - whether PCR1 contains ACPI;
   - whether MEASUREMENT equals `svsm_measurement`.
7. **Whether `--no-shielded-vtpm` works for TDX**: if it does, the vTPM device can be removed from TDX entirely, but RTMR2 would then gain LoadOptions events, so this needs to be re-evaluated.

---

## 15. Follow-ups and options
- **v1.1 metadata autostart**: for use cases that need no secrets. Metadata holds an owner-signed `ccrun-config`; `/init` measures it first, then pulls automatically, and recovers automatically after a restart. About 40 lines.
- **v2**:
  - derive CFV and TD HOB offline from the endorsed `.fd`;
  - try to reconstruct the pre-patch fw_cfg blobs from the patched ACPI tables, so that the ACPI digests can be recomputed from content;
  - an encrypted scratch disk, to support very large images;
  - A3 H100 CC.
