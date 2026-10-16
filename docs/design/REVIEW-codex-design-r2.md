## C1-C14 status

- **C1 — Fixed:** helper execution and coredumps are disabled; procfs controls, capability sets and inherited descriptors are constrained.
- **C2 — Fixed:** SNP PCR1 now requires an ACPI baseline; SVSM measurement coverage remains M0-gated.
- **C3 — Partially fixed:** TOFU is removed, but reviewed post-AML dumps are not demonstrably linked to the accepted raw ACPI digests.
- **C4 — Fixed:** manifest v1 and full AK-public-area matching are explicit and fail closed; platform support remains unproven.
- **C5 — Partially fixed:** digest binding and signed MRTD membership are specified, but the closed event table omits legitimate records.
- **C6 — Partially fixed:** status is cryptographically bound, but exec detection and consistent status snapshots remain incorrect or unspecified.
- **C7 — Partially fixed:** token destinations are restricted, but the one-time clock check does not enforce a TLS-time floor.
- **C8 — Partially fixed:** CAS and terminal failure prevent duplicate launches; evidence collection is not synchronized with state/ledger changes.
- **C9 — Fixed:** a separate single-threaded helper unshares and then forks.
- **C10 — Partially fixed:** PCR8 separation and report bindings agree; explicit validation of the signed quote type/bank/PCR selection remains missing.
- **C11 — Fixed:** replacement semantics, two-pass whiteouts, FIFO preservation and secret ownership are specified.
- **C12 — Partially fixed:** PID1 DNS and loopback are configured; DHCP renewal is explicitly deferred.
- **C13 — Partially fixed:** provisioning and basic restart paths improve, but interrupted launches and terminal guest states lack recovery rules.
- **C14 — Fixed:** native dependencies and the verifier’s membership in the relying party’s TCB are acknowledged.

## New findings

1. **High — §§4.1, 10: kernel configuration enforcement is incorrect.** `merge_config.sh -s` rejects intentional redefinitions—including enabling options over an allnoconfig base—but only warns about requested/effective mismatches. **Fix:** generate from a complete miniconfig and independently assert required-on/off values. Explicitly include `TCG_SVSM` for SVSM; when generating with `EXPERT=y` from the outset, include essentials such as `SHMEM` and `MULTIUSER`. [Linux merge script](https://raw.githubusercontent.com/torvalds/linux/v6.18/scripts/kconfig/merge_config.sh)

2. **High — §9.2.1: TCB acceptance policy disappeared.** Unlike v0.1, v0.2 does not require acceptable statuses/advisories. The selected library’s one-shot `verify()` skips those policy checks. **Fix:** require explicit policy validation, trusted verifier time and collateral freshness. Also treat `min_tcb_eval_num` as a minimum: permanently requesting that exact version eventually receives HTTP 410. [Exact library source](/home/huasheng/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/dcap-qvl-0.7.0/src/verify.rs:377), [Intel PCS rules](https://api.portal.trustedservices.intel.com/content/documentation.html)

3. **High — §6.5: the exec handshake can falsely authenticate RUNNING.** `close_range(3, ~0)` closes a precreated handshake descriptor. Even if preserved, EOF also follows child death; Linux closes CLOEXEC descriptors before all fallible exec work finishes. **Fix:** preserve the error channel, cover setup failures, coordinate reaping, and use an actual successful-exec observation—or weaken the status claim. [Linux 6.18 exec implementation](https://raw.githubusercontent.com/torvalds/linux/v6.18/fs/exec.c)

4. **Medium — §§5–7, 9.2: state and ledger rules contradict the new lifecycle.** STARTING has two events; PULLING has three; FAILED can have either prefix. §5 requires four for every non-WAITING state. Serializing evidence readers does not prevent concurrent extensions/status changes. **Fix:** define allowed state/prefix combinations and obtain a consistent status/ledger/quote snapshot. Make the config comparison conditional; WAITING has no `ledger[2]`.

5. **Medium — §§4.3, 9.2–9.3: the new event whitelist rejects genuine evidence.** Cached CCEL contains index-0 `EV_NO_ACTION` records, forbidden by the 1..3 rule. The shared table also omits SNP’s S-CRTM, NonHostInfo and tagged events; PCR5’s listed sequence omits its separator. **Fix:** handle bounded non-extending records separately, define SNP-specific digest rules, and specify PCR5 as `SEP, GPT, [Failure], Invocation, Success`.

6. **Medium — §§6.3, 13: `issued_at` is not an enforced clock floor.** `issued_at−299s` passes, permitting a recently expired certificate; subsequent TLS validations have no stated floor. Hostname allowlisting also leaves scheme/port unspecified. **Fix:** enforce at least `issued_at` during every certificate validation, define freshness limits, and authorize complete HTTPS origins.

7. **Medium — §§4.3, 9.1, 14: SVSM provisioning remains incomplete.** Removing the GA certificate-read step also removes the AK-template read needed by `CreatePrimary`; policy still assumes certificate-based instance identity although SVSM certificates are optional. **Fix:** specify template acquisition and mode-specific identity binding. Keep v1-only as an explicit simplification: v0 can bind an AK through additional registration/certification. [Google verifier](https://github.com/google/go-tpm-tools/blob/main/cmd/verify_svsm.go)

8. **Medium — §11: the advertised idempotent `up` has undefined branches.** A lost launch response leaves retries encountering STARTING/PULLING; FAILED/EXITED guests remain powered on and cannot accept another launch. **Fix:** attest and wait through intermediate states, verify against the requested configuration, and define fresh-boot recovery for terminal states.

9. **Medium — §9.1: the “self-contained” policy omits approved release identity.** Its enumerated contents exclude the PE/GPT measurements required by verification. **Fix:** embed authenticated release measurements, or bind an authenticated release digest with a defined retrieval mechanism.

10. **Medium — §7.1: optional ACPI evidence has no precise binding formula.** “Include their hashes in report_data” conflicts with the fixed formula and cannot modify SNP’s cached boot report. **Fix:** define a canonical ACPI manifest and exact binding; use fresh TPM qualifyingData for SNP.

11. **Low — §§4.2, 9.1, 14: RTMR1 retry acceptance conflicts with exact-value expectations.** §4.2 permits a seventh failure event, while M0 requires exactly six and release metadata suggests one expected RTMR1. **Fix:** define and test both accepted traces, deriving their corresponding register values.

## Blockers for TDX G3

- Establish independently trusted ACPI baselines whose reviewed contents correspond to the accepted measured blobs; signing post-execution dumps does not establish that link.
- Repair kernel generation, verifier policy/event handling, WAITING verification and consistent evidence collection; include approved release measurements.
- Complete C3 M0 and adversarial boot tests proving the single-PE boundary, external-initrd rejection, RTMR2 behavior and evidence isolation.
- Resolve G3’s timing requirement: the current protocol relies on attested init enforcing the signed config. If the verifier must **observe its measurement before pulling**, add prepare → attest → owner commit. Until then, the unconditional TDX “satisfied” claim is premature.