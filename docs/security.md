# Security and limitations

On the demo platform, vContainer trusts:

- Intel: the CPU, the TDX module and the quoting enclave;
- Google's TDX firmware, as checked against Google's signed launch endorsement;
- the host's default configuration (ACPI tables and the KVM paravirtual interface), which is trusted, not checked;
- you, the owner.

The host is otherwise untrusted: it controls the network, instance metadata, the disk, the serial console and the path to the registry. The container's isolation exists so that the container cannot forge the evidence. It is not a sandbox for untrusted code.

vContainer protects the agent's memory from the cloud host. You still need to trust the agent code: there is no built-in egress control or agent tool policy, and the agent can send data to external services. Application authentication and TLS are your responsibility.

Only secret values and the registry token are encrypted, and the container itself can still leak them. The rest is public:

- the launch configuration (image, command, environment variables and secret names), which anyone can read from the ledger;
- container output, which goes to the host-readable serial console unless you pass `--quiet`;
- traffic to the container's ports, which vContainer does not encrypt. To protect this traffic, have the container serve TLS with a key passed through `--secret`, and publish the key's fingerprint with `-e` so clients can check it against the verified evidence.

### Updating the trusted code

Security updates to `/init` or its dependencies require rebuilding and publishing the boot image as well as updating the CLI. From a clean checkout of the updated code, run `python3 scripts/install.py --publish --project PROJECT`. New sessions use that published release and its new expected measurements. Existing VMs keep their old code until they are replaced; export a new policy after redeploying. Recorded evidence for an earlier release does not establish that the updated release was tested on hardware.

The October 2026 TLS dependency update removes the affected emSign trust path using `webpki-roots` and `webpki-root-certs` 1.0.10 and `rustls-webpki` 0.103.16. See the [root-store advisory](https://rustsec.org/advisories/RUSTSEC-2026-0381.html) and [certificate-verifier advisory](https://rustsec.org/advisories/RUSTSEC-2026-0382.html). An OS trust-store update alone does not replace the roots embedded in `/init`.

### Status

Version 0.1 works end to end on real hardware. The release built from commit c2f795b was rebuilt from scratch, also in a fresh clone, and matched byte for byte.

Intel TDX on Google Cloud C3 is a demo platform, not part of the design. The design needs a measured boot, an append-only measurement register and quotes that bind data chosen by the guest. Supporting another platform means replacing the TDX and Google Cloud parts of `/init`, the verifier, the CLI's cloud commands, the kernel configuration and the image build.

[Whitepaper §8](whitepaper.md#8-security-analysis) describes the security analysis and review history.

Limits:

- One container per VM boot, with its root filesystem in RAM.
- After host maintenance the VM reboots empty: run `vcontainer run`, then `vcontainer deploy`, again.
- There is no SEV-SNP support. On Google Cloud's generally available SNP machines, the kernel can be identified only through a vTPM that the host runs. [docs/design](design/) has the analysis.

