## Verdict

The single-PE TDX architecture is a reasonable foundation, but this design does **not yet establish requirement 3**. The largest gaps are unaudited ACPI enrollment, missing SNP PCR1 appraisal, incomplete SVSM key binding, and a concrete escape from D7 through writable procfs. GA SNP honestly acknowledges its weaker root, but trusting Google’s host-managed vTPM is an exception to the original malicious-host requirement. The owner signature and per-boot HPKE binding are sound in principle; the proposed implementation and operational budgets are substantially too optimistic.

## Findings

1. **[Critical] Writable procfs defeats the evidence-protection boundary.**  
   **Where:** D7; §4.1; §6 RUNNING.  
   **Problem:** Removing capabilities and hiding devices does not protect global writable kernel controls.  
   **Scenario:** Workload root sets `/proc/sys/kernel/core_pattern` to a pipe helper such as `|/proc/%P/root/payload`, then crashes a child. Linux executes that helper as root in the initial namespaces, allowing access to PID1 and attestation interfaces. PID1’s `PR_SET_DUMPABLE=0` does not prevent this. [Linux helper behavior](https://man7.org/linux/man-pages/man5/core.5.html), [helper credentials](https://raw.githubusercontent.com/torvalds/linux/v6.18/kernel/umh.c).  
   **Recommended fix:** Disable `CONFIG_COREDUMP` and expose `/proc/sys` read-only. Specify fresh child procfs, private mount propagation, and no inherited outer directory/device descriptors. Explicitly reduce the capability **bounding set** and clear inheritable/ambient sets; otherwise subsequent exec or file capabilities can restore privileges.

2. **[High] SNP measures ACPI into PCR1 but never appraises it.**  
   **Where:** §4.3 PCR rules; §9.3; §11 “SNP needs no golden.”  
   **Problem:** Quoting and replaying PCR1 authenticates its contents without deciding whether they are acceptable. The [research notes](/home/huasheng/tdrun-design/FACTS-snp.md:99) identify three platform-configuration events followed by boot variables; the underlying SNP log labels those events `ACPI DATA`.  
   **Scenario:** Host-provided malicious AML executes before `/init`, while PCR4 still identifies the expected PE. The changed PCR1 passes replay, and all appraised PCRs can remain acceptable. An SVSM-backed TPM does not fix this omission.  
   **Recommended fix:** Add PCR1 ACPI allowlists and boot-variable constraints equivalent to TDX’s RTMR0 policy. Confirm their measurement coverage under SVSM. Remove the “no golden” and unconditional TDX-equivalence claims.

3. **[High] Collecting a golden ACPI hash does not establish that its contents are safe.**  
   **Where:** §4.2; §9.2.5; §11 `golden`; §13.1.  
   **Problem:** A fresh measurement proves which host-supplied tables executed, not that those tables were trustworthy. Automatic enrollment or TOFU leaves the hostile host selecting accepted kernel-executed code.  
   **Scenario:** The host supplies malicious AML during `golden`; its digest becomes approved, and later boots pass. Collecting tables in `/init` occurs after earlier AML execution. This is the attack class described by the [BadAML advisory](https://github.com/edgelesssys/contrast/security/advisories/GHSA-g9ww-x58f-9g6m).  
   **Recommended fix:** Make `golden` produce an enrollment proposal, requiring an independently trusted, reviewed baseline. TOFU must not produce a G3-equivalent PASS. Make the chosen AML mitigation mandatory and document its limits; checking only RAM-type SystemMemory is not automatically equivalent to comprehensive AML confinement.

4. **[High] The SVSM request does not select a manifest that binds the AK.**  
   **Where:** §4.3 SNP-SVSM; §9.3.3–4.  
   **Problem:** `service_manifest_version` defaults to zero. The initial manifest contains the EK; direct AK inclusion requires the newer manifest format. An EK alone does not authenticate an independently supplied AK. [Kernel ABI](https://www.kernel.org/doc/Documentation/ABI/testing/configfs-tsm-report), [Google verification implementation](https://github.com/google/go-tpm-tools/blob/main/cmd/verify_svsm.go).  
   **Scenario:** Correct parsing rejects the promised AK as absent; permissive parsing allows genuine service evidence to accompany an unrelated quote key.  
   **Recommended fix:** Explicitly request and require manifest version 1, validate its structure, and match the quote’s full public area to the attested AK. Reject unsupported versions. Also remove mandatory GA AK-certificate reads from the certificate-optional SVSM path.

5. **[High] Event-log replay does not authenticate event labels or arbitrary decoded claims.**  
   **Where:** §9.2.3–5; §4.3 PCR0/7/9; §9.3.  
   **Problem:** Replay authenticates extended digest sequences. Event types, descriptions and payloads need separate binding to those digests.  
   **Scenario:** An intermediary relabels a PCR9 `Linux initrd` event as `LoadOptions` without changing its digest; replay succeeds. Thus the label-based rule cannot prove `noinitrd` worked. Similarly, inspecting a supplied SecureBoot byte or Boot-variable payload without recomputing its appropriate digest authenticates nothing. [EFI measurement implementation](https://raw.githubusercontent.com/torvalds/linux/v6.18/drivers/firmware/efi/libstub/efi-stub-helper.c).  
   **Recommended fix:** Specify digest validation per event type, including Boot-variable data-only hashing and opaque-digest allowlists. Bound lengths, banks and indices; accept `0xFF` only as terminal padding. Explicitly require quoted MRTD membership in the **signed endorsement contents**, not merely fetching a signed file using an MRTD-shaped URL.

6. **[High] Fresh evidence does not authenticate “running.”**  
   **Where:** §§5–7.1; §11.  
   **Problem:** REPORTDATA binds only the nonce. JSON `state`, errors and exit status are unauthenticated; the final ledger also exists before `execve` and remains unchanged after exit.  
   **Scenario:** The host obtains fresh evidence from an exited guest, changes `state` to `running`, and passes the described checks. Namespace setup or `execve` failure after event 3 creates the same problem.  
   **Recommended fix:** Bind a canonical status snapshot into TDX REPORTDATA and SNP quote qualifyingData, consistently with the ledger snapshot. Enter RUNNING only after an exec-success handshake. Describe it as status at attestation time, not a continuing execution guarantee.

7. **[High] Private-registry credentials require trust that the threat model denies.**  
   **Where:** §1.3; §6 PULLING; §12; §13.5.  
   **Problem:** Image digests protect content integrity, not credentials forwarded after HPKE decryption. TLS PKI and time therefore affect confidentiality, not just availability.  
   **Scenario:** An unauthorized Bearer realm receives a password, or host-controlled DNS/RTC enables impersonation using an expired certificate with a compromised key. Image digest verification still succeeds afterward.  
   **Recommended fix:** Bind credentials to approved HTTPS registry/auth origins and prevent unauthorized forwarding. Either explicitly trust TLS PKI with owner-authenticated time, or use owner-pinned server keys. Narrow, short-lived owner-supplied tokens simplify this considerably. [Registry authentication protocol](https://distribution.github.io/distribution/spec/auth/token/).

8. **[Medium] One-shot launch needs an atomic transition and an irreversible failure boundary.**  
   **Where:** §6; §7.2.  
   **Problem:** Thread-per-connection handling and “accept once” do not specify synchronization.  
   **Scenario:** Two copies of a valid POST both observe WAITING, extend the ledger twice and start separate pulls. Returning to WAITING after an extension or later failure permits another inconsistent attempt.  
   **Recommended fix:** Validate bounded input, atomically reserve WAITING→STARTING, then extend and pull once. Failures after irreversible changes must be terminal. Serialize ledger/status snapshots with evidence collection.

9. **[Medium] The proposed PID-namespace launch sequence fails in a multithreaded PID1.**  
   **Where:** §6 BOOT and RUNNING.  
   **Problem:** HTTP and DHCP threads exist before PID1 calls `unshare(CLONE_NEWPID)`, which rejects a multithreaded caller. Moving it directly into `pre_exec` does not put that process into the new PID namespace; only subsequently created children enter it. [unshare semantics](https://man7.org/linux/man-pages/man2/unshare.2.html).  
   **Scenario:** Every otherwise successful deployment fails at container launch.  
   **Recommended fix:** Use `clone/clone3(CLONE_NEWPID)` or a single-threaded launch helper that unshares and then forks. Keep post-fork setup free of allocator and library-lock operations.

10. **[Medium] The normative SNP verification steps contradict the evidence format.**  
    **Where:** §4.3 versus §9.3.  
    **Problem:** Firmware-log replay over PCR0–9 includes PCR8, although runtime ledger events are absent from `binary_bios_measurements`. Section 9.3 also substitutes a request nonce for the cached report’s boot binding and names the ordinary measurement lookup instead of the SVSM-specific one. Accepted report versions disagree too.  
    **Scenario:** Valid WAITING evidence fails PCR8 replay, or implementations following different sections cannot interoperate.  
    **Recommended fix:** Replay firmware events separately from PCR8’s ledger. Define one byte-exact schema per mode, explicitly select `svsm_measurement`, and require the intended signed PCR bank/selection. A cached boot report plus fresh, correctly bound TPM quotes is otherwise defensible under the stated trust assumptions.

11. **[Medium] `tar::unpack_in` plus whiteouts is not complete OCI layer application.**  
    **Where:** D6; §6 UNPACKING; §8.  
    **Problem:** OCI requires replacement across entry types, not just ordinary tar extraction.  
    **Scenario:** A lower layer contains directory `x/y`, and an upper layer replaces `x` with a regular file. Generic extraction can fail instead of producing the specified rootfs. Opaque whiteouts must also preserve same-layer additions regardless of archive order. [OCI layer rules](https://github.com/opencontainers/image-spec/blob/main/layer.md#changeset-over-existing-files).  
    **Recommended fix:** Implement explicit replacement semantics or reuse an OCI layer applier. Keep whiteout deletion, identity-file reads, configuration injection and mount-target resolution inside the confinement boundary; image-controlled symlinks must never redirect parent-side operations. Preserve FIFOs, which are not device nodes. Chown secret files to the resolved workload UID before applying mode `0400`.

12. **[Medium] Resolver initialization is missing before registry access.**  
    **Where:** §4.1 initramfs; §6 DHCP/PULLING/UNPACKING.  
    **Problem:** PID1’s filesystem has no resolver configuration. DHCP configures addresses, MTU and routes; `resolv.conf` appears only inside the workload root after downloading. `ureq` uses the system resolver. [ureq source](https://raw.githubusercontent.com/algesten/ureq/main/src/unversioned/resolver.rs).  
    **Scenario:** Metadata access by literal IP succeeds, but the first registry hostname cannot resolve.  
    **Recommended fix:** Configure PID1’s resolver before pulling, using validated DHCP DNS addresses or an explicit resolver, and handle renewal. Bring loopback up as well.

13. **[Medium] “One command” provisioning and recovery are not fully specified.**  
    **Where:** §11; §13.6.  
    **Problem:** Firewall provisioning is a comment; `TERMINATE` does not disable automatic restart; rerunning unconditional `instances create` collides with an existing instance.  
    **Scenario:** Maintenance produces a new WAITING boot, but the documented recovery command fails or cannot reach port 7700. N2D networking also fails if provisioned with an interface unsupported by the stripped kernel.  
    **Recommended fix:** Specify GVNIC, N2D’s Milan minimum, restart behavior, owned firewall resources and cleanup. Make `up` handle an existing instance and freshly attest every resumed boot. [GCE creation requirements](https://docs.cloud.google.com/confidential-computing/confidential-vm/docs/create-a-confidential-vm-instance), [host scheduling policy](https://docs.cloud.google.com/compute/docs/instances/setting-vm-host-options).

14. **[Low] Two stated implementation assumptions are false.**  
    **Where:** G4; §9.3.1; §12.  
    **Problem:** The verifier is outside the **guest** TCB but inside the relying party’s TCB. `ring` includes C/assembly, and musl is a C library. [ring description](https://raw.githubusercontent.com/briansmith/ring/main/README.md).  
    **Scenario:** The budget is justified using an incorrect security boundary or an unattainable “zero C” claim for the selected stack.  
    **Recommended fix:** Say “Rust application code with a static native dependency set,” and count verifier parsing, cryptography and policy as security-critical.

The owner-key comparison, signed boot-id/public-key binding and HPKE composition otherwise address metadata substitution and cross-boot launch replay correctly. A genuinely read-only sysfs mount is not bypassed by `CAP_DAC_OVERRIDE`; Docker’s default set already excludes `CAP_SYS_RAWIO`.

## Cuts

- Remove TOFU-based production acceptance. Ship an authenticated, reviewed platform-policy bundle; keep `golden` as a maintainer diagnostic/enrollment tool.
- Remove UTS and IPC namespaces if PID1 does not consume their mutable state. Retain the boundaries actually protecting evidence.
- Use serialized launch handling instead of thread-per-connection state mutation.
- Sign and measure exact config bytes; separate canonical-JSON validation adds little if parsing is strict and consistent.
- Support one SVSM manifest version. Avoid adding credential-activation compatibility merely to support version zero.
- Verify and unpack layers sequentially to avoid retaining every compressed layer alongside the expanded rootfs.
- Keep HPKE when supporting secrets. Consider dropping long-lived registry-password and metadata-token modes before dropping the secure launch channel.

## Questions for the author

- **What exactly must “entire pre-pull state” mean?** DHCP, RTC, device responses and network traffic necessarily influence execution. Provide an input inventory identifying what is pinned, authenticated, rejected or safely handled as untrusted data. `CMDLINE_OVERRIDE` suppresses option interpretation, but the EFI stub still converts and measures LoadOptions.
- **Who establishes trusted ACPI baselines?** Which exact mitigation prevents malicious AML, and how are firmware/ACPI policy updates reviewed, authenticated and revoked?
- **Does M0 include adversarial boot tests?** Test altered Boot variables, `initrd=` and LoadFile2 injection, malformed LoadOptions, and benign ExitBootServices retries—not just successful boot.
- **Does G3 require a remotely observed launch-config measurement before pulling?** WAITING contains no launch config. The present protocol relies on the verified init enforcing the subsequent transition; requiring observation needs a prepare/acknowledge step.
- **Can SVSM actually provide the required v1 manifest, AK, PCR1 coverage and report-policy fields on supported GCE instances?** Keep equivalence claims gated on those results.
- **What are the concrete TCB floors and collateral expiration rules?** Cached endorsement/collateral availability alone is insufficient; the verifier needs trusted time, rollback policy and authenticated release/platform updates.
- **Does `verify` authenticate the application connection?** Currently a host can relay genuine evidence on `:7700` while substituting the service on `:8080`. The smallest scoped solution is workload TLS with an owner-pinned public key and its private key delivered through the existing sealed-secret mechanism.