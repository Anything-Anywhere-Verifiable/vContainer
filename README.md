# vcontainer

Run a container in an Intel TDX confidential VM on Google Cloud, and check what is running there before you hand it anything.

```console
$ vcontainer run --session demo
creating TDX confidential VM demo (c3-standard-4 in us-central1-a)
waiting for http://34.135.164.96:7700 to boot and answer attestation
session demo (34.135.164.96, us-central1-a): VERIFIED
  ✓ Intel TDX quote: signature valid, TCB UpToDate (evaluation 20)
  ✓ fresh quote for our nonce; state Waiting
  ✓ TD attributes: debug off (0x10000000), XFAM 0x602e7
  ✓ GCE instance 5127634889331185664 in us-central1-a (MROWNER)
  ✓ firmware: MRTD c1ee9c16e3afc506… endorsed by Google (svn 2)
  ✓ host: a Google Cloud machine (PPID in Google's host registry)
  ✓ boot image: vcontainer-77604b596a07 (kernel+initramfs+cmdline+/init, Authenticode 77604b596a07d9de…)
  ✓ ledger: fresh boot key, our owner key
  state: Waiting

$ vcontainer deploy --session demo us-central1-docker.pkg.dev/agentlockbox/vcontainer/openrouter-agent:v1 \
    -p 8080 --secret OPENROUTER_API_KEY=@openrouter.key
launching us-central1-docker.pkg.dev/agentlockbox/vcontainer/openrouter-agent@sha256:acdf834c…
launch measured and committed; the VM is pulling the image
session demo (34.135.164.96, us-central1-a): VERIFIED
  ...
  ✓ launch measured before pull: us-central1-docker.pkg.dev/.../openrouter-agent@sha256:acdf834c…
  ✓ container sealed: manifest sha256:b38d15a0…
  state: Running
  port 8080: http://34.135.164.96:8080

$ curl -X POST http://34.135.164.96:8080/chat -d '{"message":"In one sentence: what does an Intel TDX quote prove?"}'
{"model": "openai/gpt-4o-mini", "reply": "An Intel TDX quote proves ..."}
```

The whole run took about 36 s to boot and verify and 17 s to deploy, on a c3-standard-4.

## What you get

- **One verified boot image.** The VM boots a single file: a Linux kernel with a built-in initramfs and a built-in command line. The initramfs holds one static Rust program, `/init`. There is no bootloader, root disk, systemd, containerd or runc. The image is built reproducibly, and its measurement is predicted offline.
- **Verify before you send.** A new session waits empty. `vcontainer` checks Intel's quote, Google's signed firmware, the exact boot image and the session's ledger. Only then does it send the container's configuration, signed by your key. It verifies again that the VM recorded that configuration, and only then lets the VM pull the image. Everything that ran before the pull is covered by the quote. See [the whitepaper](docs/whitepaper.md).
- **Secrets only to the verified VM.** `--secret` values are encrypted (HPKE) to a key created inside the VM at boot. They appear as files in `/run/secrets`, so they never touch instance metadata, the disk or the launch configuration. The same mechanism carries the registry token for a private image.
- **Third-party verification.** `vcontainer verify --export-policy p.json` writes everything another person needs. With it they run `vcontainer verify --url http://IP:7700 --policy p.json`; they do not need your key or your cloud account.
- **Small.** About 2,600 lines of Rust (protocol 650, guest 960, CLI 950), 200 lines of shell, and a 106-option kernel configuration.

## Quick start

You need a Google Cloud project with `gcloud` logged in, Docker, and Rust (`rustup`; the repository pins 1.95.0).

One-time setup. This builds and publishes the boot image (about 10 minutes, mostly the kernel):

```sh
scripts/build-init.sh                   # /init, static musl, in a pinned build container
scripts/build-image.sh                  # kernel + initramfs + /init -> out/BOOTX64.EFI, out/disk.raw
cargo install --path crates/cli         # the vcontainer CLI
vcontainer publish                      # upload, create the GCE image, record expected measurements
```

Then:

```sh
vcontainer run --session demo                       # create (or reconnect to) a TDX VM and verify it
vcontainer deploy --session demo IMAGE -p 8080 \
    --secret API_KEY=@./key.txt -e MODEL=x -- cmd args   # 'pull' is an alias
vcontainer verify --session demo                    # verify again at any time
vcontainer logs --session demo                      # serial console: kernel, /init, container output
vcontainer ls
vcontainer rm --session demo                        # delete the VM and its firewall rule
```

- **Images.** Any linux/amd64 OCI or Docker image works. Tags are resolved to a digest before launch, and the digest is what gets measured. For Artifact Registry and gcr.io images, your `gcloud` access token is sealed to the VM so it can pull privately.
- **Network.** Port 7700 (the attestation API) and the ports you pass with `-p` are opened only to this machine's public IP. Use `--allow-from CIDR` on `run` to change that.
- **Defaults.** A session is `c3-standard-4` in `us-central1-a`. Change them with `--machine-type` and `--zone`.

The demo agent in [examples/openrouter-agent](examples/openrouter-agent) is 40 lines of Python. It answers `POST /chat` through OpenRouter, using the key from `/run/secrets/OPENROUTER_API_KEY`.

## How it works

```
your machine                                      Google Cloud C3 (Intel TDX)
vcontainer run      ── gcloud: create VM ──▶    Google firmware (MRTD, Google-signed)
                                                  └ BOOTX64.EFI = kernel + initramfs + cmdline   (RTMR1)
                                                     └ /init: ledger in RTMR3
                                                         0 boot   fresh HPKE key, boot id
                                                         1 owner  your public key (from metadata)
                    ◀── GET /v1/evidence ───     quote(nonce, status) + ledger + event log
  verify (Intel PCS, Google endorsement, release, ledger)
vcontainer deploy   ── POST /v1/launch ──▶       2 launch config ‖ H(sealed secrets) ‖ time
  verify again: the launch is measured
                    ── POST /v1/commit ──▶       pull by digest, verify, unpack to RAM
                                                  3 exec   manifest digest, then start the container
                    ◀── GET /v1/evidence ───     state: running
```

The container runs in its own mount and PID namespaces. It gets Docker's default capabilities minus `MKNOD` and `SYS_CHROOT`, and a read-only `/proc/sys` and `/sys`. It cannot see `/init`'s mounts or any TDX or TPM device. The kernel itself has no modules, no user namespaces, no io_uring, no BPF, no SysRq and no usermode helpers. This is not a sandbox: it exists so that the container cannot forge or corrupt the evidence `/init` serves.

## Trust

The design trusts:
- Intel (the CPU, the TDX module and the quoting enclave);
- Google's TDX firmware, as checked against Google's signed launch endorsement;
- the host's default configuration (ACPI tables, the KVM paravirtual interface), which is measured but not appraised;
- you, the owner.

The host is otherwise untrusted: it controls the network, instance metadata, the disk, the serial console and the registry path. The [whitepaper](docs/whitepaper.md) lists what a PASS establishes, what it does not, and why.

## Layout

```
crates/core    protocol: ledger, evidence, launch/commit, HPKE, Ed25519, OCI registry client
crates/init    guest PID 1: boot, RTMR3, quotes, API, pull/unpack, start the container
crates/cli     vcontainer: run, deploy, verify, ls, logs, rm, measure, publish
image/         kernel configuration (allnoconfig + 106 options) and its checks
scripts/       build-init.sh, build-image.sh, qemu-boot.sh
tools/         tdx-probe (hardware probe used to validate the platform), predict-rtmr1.py
examples/      openrouter-agent
docs/          whitepaper.md
```

## Status

Version 0.1, Intel TDX on Google Cloud C3 only. It works end to end on real hardware. The code was reviewed by Codex and an adversarial multi-agent review.

There is no SEV-SNP support. On Google Cloud's generally available SNP machines, the kernel can be identified only through a vTPM that the host runs. An SVSM-based mode is in alpha. See [docs/design](docs/design/) for the analysis.

One container runs per session, and its root filesystem lives in RAM. After host maintenance, a session reboots empty; run `vcontainer deploy` again.
