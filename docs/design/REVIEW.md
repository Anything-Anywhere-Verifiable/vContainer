# v0.1 review dispositions

This review covered v0.1 of the design (not included in this repository). Every disposition is reflected in [DESIGN.md](DESIGN.md) (v0.2).

The reviews came from two sources:
- **Codex**: the original is in [REVIEW-codex-design-r1.md](REVIEW-codex-design-r1.md). Its findings are numbered C1–C14 below.
- **Claude**: reviewed from 4 perspectives (TDX G3 completeness, SNP, protocol and isolation, minimality/usability/feasibility). Each perspective was paired with a challenger who rechecked every finding, 39 findings in total. The severities below are the conclusions **after** the recheck.

Dispositions:
- **Accepted**: fixed as the finding recommends.
- **Partially accepted**: the valid part was adopted.
- **Rejected**: the reason is given in the table.

## High / critical

| Finding | Source | Disposition | Location in v0.2 |
|---|---|---|---|
| The workload rewrites `/proc/sys/kernel/core_pattern`, and the kernel runs the helper it names as root in the initial namespace | C1 (Critical); the Claude challenger also reproduced it independently | **Accepted**: `STATIC_USERMODEHELPER` with an empty path (disables all usermode helpers), `COREDUMP=n`, `/proc/sys` read-only, and sysrq-trigger, kcore and similar files are masked as well | §4.1, §6.5 |
| The host can trigger SysRq through a serial BREAK or PS/2 and dump the GPRs | Claude tdx-g3 (high, confirmed) | **Accepted**: `MAGIC_SYSRQ=n`, `VT=n`, `INPUT_KEYBOARD=n`; the workload's stdin is connected to `/dev/null`; PID1's fds 0–2 point to `/dev/null` | §4.1, §4.4, §6.2, §6.5 |
| A workload with `CAP_SYS_CHROOT` can escape by repeated chroot and reach the inherited configfs, writable sysfs and TEE devices | Claude protocol (downgraded to medium on recheck) | **Accepted**: in a child mount namespace, `umount2(MNT_DETACH)` removes all of init's mounts; switch to `MS_MOVE`+chroot; drop `SYS_CHROOT` | §6.5 |
| SNP has no rules at all for PCR1 (host ACPI) or PCR2/3/6 (Driver####, option ROMs) | C2; Claude snp ×2 (high, confirmed) | **Accepted**: PCR1 now uses the same ACPI baseline as TDX; PCR2/3/6 are pinned exactly to constants that contain only SEP; the claim that "SNP needs no golden" is removed | §4.3, §9.3 |
| Golden and TOFU just blindly accept the host's ACPI | C3; Claude (downgraded to medium on recheck) | **Accepted**: interactive TOFU is removed; golden only produces a proposal (with an iasl dump, a diff and automated checks), which takes effect only after a human reviews and signs it; the BadAML patch is mandatory; the docs now say "pin" instead of "audit" | D11, §9.1, §11, §13 |
| If SVSM is not given a manifest version, it defaults to v0, which contains only the EK and cannot bind the AK | C4; Claude snp (medium, confirmed) | **Accepted**: `service_manifest_version=1`; compare the full TPMT_PUBLIC and require correct attributes; if v1 is not available, drop SVSM mode | §4.3, §9.3, §14 |
| Decisions are made on event labels (PCR9, SecureBoot, etc.), but the labels are not covered by the digest | C5; Claude snp (low) | **Accepted**: bind the digest by event type (H(data), H(VariableData) or opaque comparison); PCR9 is no longer evaluated, and `noinitrd` is guaranteed by construction; MRTD is explicitly required to appear in the endorsement's signed contents | §9.2 step 5, §4.3 |
| The `state` in the evidence is not covered by the signature, so the host can change exited to running | C6 | **Accepted**: status is bound into REPORTDATA or qualifyingData; an exec handshake is added; FAILED is terminal | §6.1, §6.5, §7.1 |
| Confidentiality of private-registry credentials depends on TLS PKI and the clock | C7; Claude (low) | **Accepted**: short-lived tokens only; tokens are sent only to the named registry host and an allowlist of realms; an owner-signed `issued_at` is used as a lower bound for the RTC | §6.3, §13 |
| The trust statement for `snp-hostvtpm` is inaccurate: the TIS MMIO is emulated by the VMM, so the whole host must be trusted, not just the vTPM | Claude snp (downgraded to medium on recheck) | **Accepted**: the wording is now "trusts the entire Google host", and the result is labeled `root=amd+google-host`; a policy without `allow_host_vtpm` FAILs | §0, §1.3, §4.3 |
| The policy does not fix the TEE mode, so there is a downgrade risk | Claude snp (high, partly valid) | **Accepted**: `policy.tee` is now required; the modes use mutually exclusive measurement-selection rules | §9.1, §9.3 |

## Medium

| Finding | Source | Disposition | Location in v0.2 |
|---|---|---|---|
| Starting from defconfig pulls in many drivers that parse host devices, and `=n` gets turned back on by `select` (for example ATA→SCSI) | Claude ×2 | **Accepted**: switch to allnoconfig, a positive allowlist and `merge_config.sh -s`, plus must-be-off assertions | D10, §4.1 |
| `VIRT_DRIVERS` was left out, so the TDX/SNP drivers are silently dropped | Claude (confirmed) | **Accepted**: added to the allowlist, with strict merging as a backstop | §4.1 |
| `unshare(CLONE_NEWPID)` in a multithreaded PID1 is a problem | C9 | **Accepted**: a single-threaded launch helper now does the `unshare` and then forks | §6.5 |
| PID1 has no resolv.conf; `lo` is not brought up | C12; Claude ×2 | **Accepted** | §4.1, §6.2, §6.4 |
| OCI replacement semantics (a directory and a file replacing each other, hard links emitted again) and the ordering of opaque whiteouts | C11; Claude ×2 | **Accepted**: two passes per layer, lstat first, then delete | §6.4 |
| gzip decodes only the first member and zstd only a single frame, so content is silently truncated and differs from what Docker sees | Claude (confirmed) | **Accepted**: use `MultiGzDecoder`; read to EOF and require the blob to be fully consumed | §6.4, §8 |
| launch has no atomic transition | C8; Claude (low) | **Accepted**: first run the side-effect-free checks, then CAS; any failure after that goes to a terminal state | §6.3 |
| `up` is not idempotent and cannot recover after host maintenance; the creation parameters differ from the tested combination; the default service account is attached | C13; Claude ×2 | **Accepted**: `up` is now idempotent, uses the fixed tested parameters, defaults to `--no-service-account --no-scopes`, and uses a separate firewall rule per instance | §11 |
| Golden is keyed by MRTD, so every firmware rotation invalidates it; third parties do not have platform.json | Claude (partly valid) | **Accepted**: ACPI is now keyed by machine type; `up` copies the relevant baseline into a self-contained policy | §9.1 |
| The SNP specification contradicts itself (PCR8 is put into the firmware log replay; REPORT_DATA is written inconsistently) | C10; Claude (low) | **Accepted**: PCR8 is replayed separately; boot_id is always taken from `ledger[0]` | §4.3, §9.3 |
| MROWNER instance binding is optional, so a third party can use the owner's public key to hijack the launch | Claude (low) | **Accepted**: `up` enforces the check and writes it into the policy | §4.2 |
| Accepting only `UpToDate` means the whole fleet FAILs once Intel publishes a TCB recovery | Claude (low) | **Accepted**: fetch collateral at a fixed eval number; a newer status only produces a warning | §9.2 |
| tmpfs peak memory use is too high; download size has no limit | Claude (low) | **Accepted**: process layers one at a time as a pipeline, and abort once `descriptor.size+1` bytes have been read | §6.3 |

## Low, rejected or deferred

| Finding | Disposition |
|---|---|
| G4's claim of "zero C dependencies" is untrue (ring, musl) | Accepted; G4 rewritten |
| config and env are in fact public, and §0 and §13 do not say so clearly | Accepted; now stated in §0, §7.2, §13 |
| RTMR1 has a legitimate EBS-failure variant | Accepted; RTMR1 now uses a syntax check that allows one `EBS Failed` event |
| If the DHCP server replies by unicast, the reply is not received | Accepted; the BROADCAST flag is set; added to M0 |
| Inputs such as SMBIOS and CPUID 0x4000_00xx are not fully listed | Accepted: `DMI=n`, `nopv`; a new input inventory in §4.4 |
| The secret ciphertext is not recorded in the ledger | Partially accepted: ledger #2 now includes `SHA-512(enc‖ct)` for after-the-fact audit |
| TD HOB and CFV can be derived offline, without going through golden | Deferred to v2 (about 150 lines); v1 uses a reviewed baseline |
| The application connection is not authenticated (the host can forward genuine evidence on :7700 while replacing :8080) | Accepted with a minimal fix: config gains `tls_spki_sha256`, and the private key is delivered through `--secret` |
| Inherited fds leak the TPM and sysfs | **Rejected** (the challenger already rejected it on recheck): Rust std uses O_CLOEXEC by default. One `close_range` line was still added as insurance |
| Capabilities should be an empty allowlist | **Rejected** (the challenger already rejected it on recheck): it would break common images that use gosu, chown and the like, which conflicts with G1; only `MKNOD` and `SYS_CHROOT` are dropped |
| Kernel LPE attack surface (io_uring, userfaultfd, BPF) | Partially accepted: allnoconfig plus assertions covers it; no extra code is needed |
| Make metadata-driven self-start the default mode (Claude cut) | **Rejected as the default**: the agent needs secrets, and the core of G3 is "verify before delivering". Listed as a v1.1 option (§15) |
| Remove SNP-vTPM entirely (Claude cut) | **Partially accepted**: the owner explicitly wants an SNP option, so it stays, but it is renamed `snp-hostvtpm` and requires explicit authorization; whether SNP ships is decided by M0 item 6 |
| Remove MROWNER and instance binding (Claude cut) | Rejected: it costs about 10 lines and prevents hijacking by parties other than Google |
| Remove init's self-check of the initial register values (Claude cut) | Accepted: the verifier's replay from zero already fails closed |
| UTS/IPC namespaces add little (C cut) | Accepted; only NEWNS and NEWPID are kept |
| For SNP, get the VCEK and endorsements only from KDS/GCS plus a cache, not from auxblob (Claude cut) | Accepted (the contents of auxblob have not yet been checked on real hardware) |
| Drop `SEV_CAPABLE` (Claude cut) | Accepted |

## Open questions for the owner
1. **Whether to ship SNP**: this depends on M0 item 6 (whether SVSM manifest v1 can be obtained). If SVSM is not available, should the release be "TDX only", or "also ship `snp-hostvtpm` with a warning"?
2. **Who reviews the ACPI baseline, and how often it is updated**: this is the main operational cost of the TDX design. One model is Tinfoil's hardware-measurements repository, with releases signed by the maintainers.
3. **Kernel version**: 6.18 LTS with MS_MOVE+chroot, or go straight to 7.x (it has nullfs, so pivot_root can be used, but it is not LTS)?

---

# Round 2: Codex review of v0.2 ([REVIEW-codex-design-r2.md](REVIEW-codex-design-r2.md)) → v0.3

Codex's recheck of the 14 round-one findings: C1, C2, C4, C9, C11 and C14 are fixed; C3, C5, C6, C7, C8, C10, C12 and C13 are partly fixed. The new findings in this round and their dispositions:

| # | Finding | Disposition (v0.3) |
|---|---|---|
| N1 | `merge_config.sh -s` only warns, and does not fail, when the requested value ≠ the effective value; allnoconfig is also missing basic options such as `SHMEM`, `MULTIUSER` and `TCG_SVSM` | Switch to `allnoconfig KCONFIG_ALLCONFIG=miniconfig`, plus a separate assertion script; add the missing basic options; the final test is that M0 boots (§4.1, §10) |
| N2 | The TCB policy was lost in v0.2; fetching collateral by an old eval version will eventually hit HTTP 410 | We check the status and advisories explicitly, and add `nextUpdate`; the eval version is a lower bound; `tcb_grace_until` carries a deadline (§9.2 step 1) |
| N3 | `close_range` also closes the handshake pipe; the parent also reads EOF when the child exits | Use `CLOSE_RANGE_CLOEXEC`; the failure path writes `(step, errno)`; the meaning of `running` is narrowed to "execve was committed, and the process was still alive when the quote was taken", confirmed with `waitpid(WNOHANG)` (§6.5) |
| N4 | The state and the ledger prefix contradict each other; snapshots are inconsistent | Add a table mapping each state to its ledger prefix; extend, state changes and evidence generation share one lock; config is compared only when #2 is present in the ledger (§5, §9.2) |
| N5 | The event allowlist rejects real evidence: `NO_ACTION` at index 0, SNP's S-CRTM/NonHostInfo, and the SEP in PCR5 | `NO_ACTION` is handled separately; the SNP event types are added; the PCR5 syntax follows what M0 measures (§4.3, §9.2 steps 4–5) |
| N6 | `issued_at − 299s` still passes; the origin does not include the scheme and port | Set a time floor with `clock_settime(max(RTC, issued_at))`; the unit of authorization is now the full HTTPS origin (§6.3) |
| N7 | SVSM mode also needs to read the AK template; there is no instance identity under SVSM | Both modes read the template (the NV index is to be confirmed by M0); the instance binding for `snp-svsm` is empty (§4.3, §9.1) |
| N8 | The behavior of `up` in the STARTING, PULLING, FAILED and EXITED states is undefined | Every state now has a defined branch; `--recreate` is added, which gets a fresh boot through a reset (§11) |
| N9 | The "self-contained" policy lacks the release identity | Copy the PE and GPT digests, both RTMR1 traces and every expected PCR value into the policy (§9.1) |
| N10 | `acpi=1` lacks an exact binding formula | Define a canonical `acpi_manifest` and how it is bound under TDX/SNP, and state that it has no cryptographic link to RTMR0 (§7.1) |
| N11 | The RTMR1 retry trace contradicts M0's "exactly 6 events" | release.json gives both traces (§4.2, §14) |
| Blocker | G3 timing: can the remote side see the config measurement before the pull? | **Accepted**: a new two-phase prepare → attest → commit flow (STARTING state + `POST /v1/commit`), about 25 lines, done automatically by the CLI (§0, §6.3, §7.3) |
| Blocker | There is no cryptographic link between the reviewed ACPI content and the RTMR0 digest | **Accepted as a declared residual risk**: the TDX conclusion becomes "G3 holds, given trust in the reviewed ACPI digest"; actually establishing the link requires recovering the pre-patch blob, which is listed for v2 (§13.1, §15) |
| Blocker | M0 and the adversarial boot tests are not done yet | Still a prerequisite for starting work (§14) |
