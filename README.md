# vcontainer

vcontainer runs a container in an Intel TDX confidential VM on Google Cloud. Before your image, secrets or data reach the VM, it proves to you exactly what the VM is running.

The VM boots a single file: a Linux kernel with a built-in initramfs whose only program is a small static Rust `/init`. There is no bootloader, root disk, systemd or container runtime to trust. The file is built reproducibly, and its measurement is predicted offline.

```sh
vcontainer run --session demo                      # boot a TDX VM and verify it
vcontainer deploy --session demo IMAGE -p 8080     # send the container to the verified VM
vcontainer verify --session demo                   # verify again at any time
```

## How it works

1. **Boot and measure.** `run` creates a C3 TDX VM. Before any network I/O, `/init` records a fresh boot key in the TDX register RTMR3. It then records your public key, read from instance metadata.
2. **Verify the empty VM.** The CLI fetches a quote over a fresh nonce. It checks Intel's signature and TCB status, Google's signed firmware, the exact boot image, and the RTMR3 ledger.
3. **Launch, verify, commit.** `deploy` sends a launch signed with your key. The launch carries the image digest, the container settings, and secrets encrypted (HPKE) to the boot key. `/init` records the launch in RTMR3. The CLI checks that the recorded launch is the one it sent, and only then lets the VM pull.
4. **Pull and run.** The VM pulls the image by digest and checks every layer. It unpacks the image into RAM, records the manifest digest, and starts the container in its own namespaces. Secrets appear as files in `/run/secrets`.

As a result, the quote covers everything that ran before the pull, and secrets reach only a VM you have verified. The container cannot reach the quote interface or write to RTMR3, so it cannot forge the evidence. Anyone you give an exported policy can verify the session. They need neither your key nor your cloud account.

## Install

Requirements:
- Linux x86-64 with Docker and `rustup` (the repository pins Rust 1.95.0).
- A Google Cloud project with the Compute Engine API enabled and C3 quota in your zone.
- `gcloud`, logged in, with a default project set (`gcloud config set project PROJECT`).

```sh
git clone https://github.com/Mengyuan-L/vcontainer && cd vcontainer
scripts/build-init.sh             # /init, static musl, built in a pinned container
scripts/build-image.sh            # Linux 6.18.57 (sha256-checked) + /init -> out/BOOTX64.EFI, out/disk.raw
cargo install --path crates/cli   # the vcontainer CLI
vcontainer publish                # create the GCE image and record its expected measurements
```

You do this once per release. It takes about 10 minutes, mostly building the kernel.

The build scripts refuse a dirty tree; pass `--dev` while hacking. Every release is therefore a commit that anyone can rebuild byte for byte. `verify` prints that commit, and `vcontainer measure` prints the expected measurements without publishing.

Your owner key and session files live in `~/.config/vcontainer`. The key is created on first use.

## Usage

```console
$ vcontainer run --session agent
creating TDX confidential VM agent (c3-standard-4 in us-central1-a)
waiting for http://34.28.55.190:7700 to boot and answer attestation
session agent (34.28.55.190, us-central1-a): VERIFIED
  ✓ Intel TDX quote: signature valid, TCB UpToDate (evaluation 20)
  ✓ fresh quote for our nonce; state Waiting
  ✓ TD attributes: debug off (0x10000000), XFAM 0x602e7
  ✓ GCE instance 1839456890140757969 in us-central1-a (MROWNER)
  ✓ firmware: MRTD c1ee9c16e3afc506… endorsed by Google (svn 2)
  ✓ host: a Google Cloud machine (PPID in Google's host registry)
  ✓ boot image: kernel+initramfs+cmdline+/init with Authenticode b0a1735c…7e33db0 (release vcontainer-b0a1735ce42a, commit 55d07e68…)
  ✓ ledger: fresh boot key, our owner key
  state: Waiting

$ vcontainer deploy --session agent us-central1-docker.pkg.dev/P/R/openrouter-agent:v1 \
    -p 8080 --secret OPENROUTER_API_KEY=@openrouter.key
launching us-central1-docker.pkg.dev/P/R/openrouter-agent@sha256:acdf834c…
launch measured and committed; the VM is pulling the image
session agent (34.28.55.190, us-central1-a): VERIFIED
  ...
  ✓ launch measured before pull: us-central1-docker.pkg.dev/P/R/openrouter-agent@sha256:acdf834c…
  ✓ container sealed: manifest sha256:b38d15a0…
  state: Running
  port 8080: http://34.28.55.190:8080
```

On a `c3-standard-4`, `run` takes about 36 s and `deploy` about 20 s. The agent in [examples/openrouter-agent](examples/openrouter-agent) is 40 lines of Python.

| Command | What it does |
|---|---|
| `run --session NAME` | Create a TDX VM, or reconnect to an existing one, and verify it. `--zone` (default `us-central1-a`), `--machine-type` (default `c3-standard-4`), `--allow-from CIDR` (default: your public IP). |
| `deploy --session NAME IMAGE [-- CMD ARGS]` | Verify, launch, verify again, then pull and start the image. Also available as `pull`. |
| `verify --session NAME` | Verify again. `--export-policy p.json` writes what a third party needs; they then run `vcontainer verify --url http://IP:7700 --policy p.json`. |
| `logs --session NAME` | Print the serial console: kernel, `/init` and container output. |
| `ls`, `rm --session NAME` | List sessions; delete a session's VM and firewall rule. A session bills as a running VM until you `rm` it. |

`deploy` options:
- `-p PORT` opens a container port to the `--allow-from` range.
- `-e KEY=VALUE` sets an environment variable. Variables are measured and appear in `verify` output, so do not use them for secrets.
- `--secret NAME=@FILE`, `NAME=VALUE` or `NAME` (taken from your environment) seals a secret to the VM. It appears as `/run/secrets/NAME`.
- `--user`, `--workdir` and `--entrypoint` override the image's settings.
- `--quiet` keeps container output off the serial console. By default output goes there, where the host can read it.
- `--resume` finishes a deploy that was interrupted after its launch was sent.

Any linux/amd64 OCI or Docker image works. A tag is resolved to a digest before launch, and the digest is what gets measured. For Artifact Registry and gcr.io images, your `gcloud` access token is sealed to the VM so it can pull private images.

## Trust

vcontainer trusts:
- Intel: the CPU, the TDX module and the quoting enclave;
- Google's TDX firmware, as checked against Google's signed launch endorsement;
- the host's default configuration (ACPI tables and the KVM paravirtual interface), which is measured but not appraised;
- you, the owner.

The host is otherwise untrusted: it controls the network, instance metadata, the disk, the serial console and the path to the registry. The container's isolation exists so that the container cannot forge the evidence. It is not a sandbox for untrusted code.

## Status

Version 0.1 supports Intel TDX on Google Cloud C3 only. It works end to end on real hardware. The release built from commit 55d07e6 was rebuilt from scratch and matched byte for byte.

The code had three Codex review rounds and one adversarial multi-agent review. Every confirmed finding was fixed; [whitepaper §8](docs/whitepaper.md#8-security-analysis) summarizes them.

Limits:
- One container runs per session, and its root filesystem lives in RAM.
- After host maintenance the VM reboots empty. Run `run`, then `deploy`, again. Until then `verify` reports that the session was not deployed again.
- There is no SEV-SNP support. On Google Cloud's generally available SNP machines, the kernel can be identified only through a vTPM that the host runs. [docs/design](docs/design/) has the analysis.

## Learn more

- [docs/whitepaper.md](docs/whitepaper.md) presents the design top-down: what a PASS establishes, what it does not, and why.
- [image/README-image.md](image/README-image.md) covers the kernel configuration and the platform facts measured on real C3 hardware.
- Code, about 2,600 lines of Rust:
  - [crates/core](crates/core): the protocol and the OCI client;
  - [crates/init](crates/init): the guest `/init`;
  - [crates/cli](crates/cli): the CLI and verifier.
