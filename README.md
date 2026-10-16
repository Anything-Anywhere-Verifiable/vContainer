# vContainer

vContainer is a minimal implementation for running an AI agent in the cloud confidentially and verifiably. It runs the agent, or any other container, in a confidential VM that you can verify before you trust it.

An agent in the cloud holds API keys and often handles private data. On an ordinary VM, the cloud host can read both. A confidential VM keeps its memory encrypted from the host, and the hardware signs a quote of what the VM booted. vContainer builds on this:
- you verify the VM's firmware, boot image and empty state before the agent's image or keys reach it;
- the agent's API keys are encrypted so that only the verified VM can read them;
- others, such as the agent's users, can check the same evidence with a policy file you give them.

vContainer does this with as little code as possible:

| Part | Size |
|---|---|
| Code we wrote that runs in the VM (`/init` and the protocol) | about 1,600 lines of Rust |
| CLI and verifier, on your machine | about 1,000 lines of Rust |
| Kernel configuration | `allnoconfig` plus 109 options |
| Build scripts | about 200 lines of shell |

The VM boots a single file: a Linux kernel with a built-in initramfs whose only program is `/init`. There is no bootloader, root disk, systemd or container runtime. Until a container is deployed, the VM runs only the kernel and `/init`, which links musl and crates such as rustls, hpke and tar. The build is designed to be reproducible, and the file's measurement is predicted offline.

## How it works

The steps below are for the demo platform, Intel TDX on Google Cloud C3.

1. **Boot and measure.** `run` creates a C3 TDX VM. `/init` first records a fresh boot key in the TDX register RTMR3. It then fetches your public key from instance metadata and records it too.
2. **Verify the empty VM.** The CLI fetches a quote over a fresh nonce. It checks Intel's signature and TCB status, Google's signed firmware, the exact boot image, and the RTMR3 ledger.
3. **Launch, verify, commit.** `deploy` sends a launch signed with your key. The launch carries the image digest, the container settings, and secrets encrypted (HPKE) to the boot key. `/init` records the launch in RTMR3. The CLI checks that the recorded launch is the one it sent, and only then lets the VM pull.
4. **Pull and run.** The VM pulls the image by digest and checks every layer. It unpacks the image into RAM, records the manifest digest, and starts the container in its own namespaces. Secrets appear as files in `/run/secrets`.

As a result, you have verified the VM's boot and its empty state before it pulls anything, and secrets reach only that VM. The container cannot request quotes over its own data or write to RTMR3, so it cannot forge the evidence.

## Install

Requirements:
- Linux x86-64 with Docker and `rustup` (the repository pins Rust 1.95.0).
- A Google Cloud project with the Compute Engine API enabled and C3 quota in your zone.
- `gcloud`, logged in, with a default project set (`gcloud config set project PROJECT`).

```sh
git clone https://github.com/Mengyuan-L/vContainer && cd vContainer
scripts/build-init.sh             # /init, static musl, built in a pinned container
scripts/build-image.sh            # Linux 6.18.57 (sha256-checked) + /init -> out/BOOTX64.EFI, out/disk.raw
cargo install --path crates/cli   # the vcontainer CLI
vcontainer publish                # create the GCE image and record its expected measurements
```

You do this once per release. It takes about 10 minutes, mostly building the kernel.

The build scripts refuse uncommitted changes in `crates/`, `image/` and `scripts/`; pass `--dev` while hacking. They write the commit to `out/build-info.json`, so a release can be rebuilt from its commit, and `verify` prints that commit. `vcontainer measure` prints the expected measurements without publishing.

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

On a `c3-standard-4`, `run` takes about 36 s, and deploying the 43 MB demo image takes about 20 s. The demo agent in [examples/openrouter-agent](examples/openrouter-agent) is 40 lines of Python. It answers chat requests through OpenRouter with a key read from `/run/secrets`.

| Command | What it does |
|---|---|
| `run --session NAME` | Create a TDX VM, or reconnect to an existing one, and verify it. `--zone` (default `us-central1-a`), `--machine-type` (default `c3-standard-4`), `--allow-from CIDR` (default: your public IP). |
| `deploy --session NAME IMAGE [-- CMD ARGS]` | Verify, launch, verify again, then pull and start the image. Also available as `pull`. |
| `verify --session NAME` | Verify again. `--export-policy p.json` writes what a third party needs. They run `vcontainer verify --url http://IP:7700 --policy p.json`, from an address that `--allow-from` admits. |
| `logs --session NAME` | Print the serial console: kernel, `/init` and container output. |
| `ls`, `rm --session NAME` | List sessions; delete a session's VM and firewall rule. A session bills as a running VM until you `rm` it. |

`deploy` options:
- `-p PORT` opens a container port to the `--allow-from` range.
- `-e KEY=VALUE` sets an environment variable. Variables are public (see [Trust](#trust)).
- `--secret NAME=@FILE`, `NAME=VALUE` or `NAME` (taken from your environment) seals a secret to the VM. It appears as `/run/secrets/NAME`.
- `--user`, `--workdir` and `--entrypoint` override the image's settings.
- `--quiet` keeps container output off the serial console.
- `--resume` commits a launch that an interrupted `deploy` already sent. Repeat its `-p` options.

Images must be linux/amd64 and fit in RAM. The kernel is minimal; for example, it has no user namespaces or io_uring. A tag is resolved to a digest before launch, and the digest is what gets measured. Private images work on Artifact Registry and gcr.io, where your `gcloud` access token is sealed to the VM.

## Trust

On the demo platform, vContainer trusts:
- Intel: the CPU, the TDX module and the quoting enclave;
- Google's TDX firmware, as checked against Google's signed launch endorsement;
- the host's default configuration (ACPI tables and the KVM paravirtual interface), which is trusted, not checked;
- you, the owner.

The host is otherwise untrusted: it controls the network, instance metadata, the disk, the serial console and the path to the registry. The container's isolation exists so that the container cannot forge the evidence. It is not a sandbox for untrusted code.

Only secret values and the registry token are encrypted, and the container itself can still leak them. The rest is public:
- the launch configuration (image, command, environment variables and secret names), which anyone can read from the ledger;
- container output, which goes to the host-readable serial console unless you pass `--quiet`;
- traffic to the container's ports, which vContainer does not encrypt. The demo agent serves plain HTTP. To protect this traffic, have the container serve TLS with a key passed through `--secret`. Publish the key's fingerprint with `-e`, so clients can check it against the verified evidence.

## Status

Version 0.1 works end to end on real hardware. The release built from commit 55d07e6 was rebuilt from scratch and matched byte for byte.

Intel TDX on Google Cloud C3 is a demo platform, not part of the design. The design needs a measured boot, an append-only measurement register and quotes that bind data chosen by the guest. Supporting another platform means replacing the TDX and Google Cloud parts of `/init`, the verifier, the CLI's cloud commands, the kernel configuration and the image build.

The code had three Codex review rounds and one adversarial multi-agent review. Every confirmed finding was fixed; [whitepaper §8](docs/whitepaper.md#8-security-analysis) summarizes them.

Limits:
- One container runs per session, and its root filesystem lives in RAM.
- After host maintenance the VM reboots empty. Run `run`, then `deploy`, again. Until you `run`, `verify` reports that the container was not redeployed.
- There is no SEV-SNP support. On Google Cloud's generally available SNP machines, the kernel can be identified only through a vTPM that the host runs. [docs/design](docs/design/) has the analysis.

## Learn more

- [docs/whitepaper.md](docs/whitepaper.md) presents the design top-down: what a PASS establishes, what it does not, and why.
- [image/README-image.md](image/README-image.md) covers the kernel configuration and the platform facts measured on real C3 hardware.
- Code:
  - [crates/core](crates/core): the protocol and the OCI client;
  - [crates/init](crates/init): the guest `/init`;
  - [crates/cli](crates/cli): the CLI and verifier.
