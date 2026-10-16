# vContainer

vContainer is a minimal implementation for running an AI agent in the cloud confidentially and verifiably. It runs the agent, or any other container, in a confidential VM that you can verify before you trust it.

An agent in the cloud holds API keys and often handles private data. On an ordinary VM, the cloud host can read both. A confidential VM keeps its memory encrypted from the host, and the hardware signs a quote of what the VM booted. vContainer builds on this:

- you verify the VM's firmware, boot image and empty state before the agent's image or keys reach it;
- the agent's API keys are encrypted so that only the verified VM can read them;
- others, such as the agent's users, can check the same evidence with a policy file you give them.

vContainer does this with as little code as possible:

| Part | Size |
|---|---|
| Code we wrote that runs in the VM (`/init` and the protocol) | about 1,450 lines of Rust |
| CLI and verifier, on your machine | about 950 lines of Rust |
| Kernel configuration | `allnoconfig` plus 107 options |
| Build script | about 110 lines of shell |

The VM boots a single file: a Linux kernel with a built-in initramfs whose only program is `/init`. There is no bootloader, root disk, systemd or container runtime. Until a container is deployed, the VM runs only the kernel and `/init`. The build is designed to be reproducible, and the file's measurement is predicted offline.

## How it works

```
your machine                                   confidential VM
─────────────────────────────────────────────────────────────────────────────
vcontainer run      ─── create VM ──────▶      boots the single measured file
                    ◀── quote + ledger ──      /init records a fresh key and your key
  verify: hardware, firmware, boot image, ledger

vcontainer deploy   ─── signed launch ──▶      /init records the launch
                    ◀── quote + ledger ──
  verify: the recorded launch is the one we sent
                    ─── signed commit ──▶      pulls the image by digest, verifies it,
                                               starts the container; secrets in /run/secrets
```

1. **Boot.** `vcontainer run` creates the VM. Before it does anything else, `/init` records a fresh key for this boot and your public key in a hardware measurement register that only grows.
2. **Verify.** Your machine asks the VM for a quote over a fresh nonce and checks it: the hardware vendor's signature, the cloud's signed firmware, the exact boot image, and the register. At this point the VM is verified and empty.
3. **Launch.** `vcontainer deploy` sends the container's configuration, signed with your key, with the secrets encrypted to the key from step 1. `/init` records the launch in the register. Your machine verifies again that the recorded launch is the one it sent, and only then lets the VM pull.
4. **Run.** The VM pulls the image by digest, checks every layer, and starts the container. Secrets appear as files in `/run/secrets`.

So the quote covers everything that happened before the pull, and the secrets reach only the VM you verified. The container cannot write to the register or request quotes over its own data, so it cannot forge the evidence.

The demo platform is Intel TDX on Google Cloud C3; see [Status](#status) for what is platform-specific.

## Install

You need:

- Linux x86-64 with Docker and `rustup` (the repository pins Rust 1.95.0);
- a Google Cloud project with the Compute Engine API enabled and C3 quota in your zone;
- `gcloud`, logged in, with a default project set (`gcloud config set project PROJECT`).

```sh
git clone https://github.com/Mengyuan-L/vContainer && cd vContainer
scripts/build-image.sh            # builds /init and the kernel in a pinned container -> out/BOOTX64.EFI, out/disk.raw
cargo install --path crates/cli   # the vcontainer CLI
vcontainer publish                # creates the GCE image and records its expected measurements
```

You do this once per release. The first build downloads the kernel source and the build container's packages.

`scripts/build-image.sh` refuses a checkout with uncommitted changes; pass `--dev` while hacking. A release builds the kernel from freshly extracted, sha256-checked source, and anyone can rebuild it byte for byte from its commit, which `vcontainer publish` records and `vcontainer verify` prints. `vcontainer measure` prints the expected measurements without publishing.

Your owner key and session files live in `~/.config/vcontainer`. The key is created on first use.

## Usage

A session is one VM. Start it, deploy a container into it, and delete it when you are done.

```console
$ vcontainer run --session agent
creating TDX confidential VM agent (c3-standard-4 in [ZONE])
waiting for http://[IP]:7700 to boot and answer attestation
session agent ([IP], [ZONE]): VERIFIED
  ✓ Intel TDX quote: signature valid, TCB UpToDate (evaluation 20)
  ✓ fresh quote for our nonce; state Waiting
  ✓ TD attributes: debug off (0x10000000), XFAM 0x602e7
  ✓ GCE instance [ID] in [ZONE] (MROWNER)
  ✓ firmware: MRTD c1ee9c16e3afc506… endorsed by Google (svn 2)
  ✓ host: a Google Cloud machine (PPID in Google's host registry)
  ✓ boot image: kernel+initramfs+cmdline+/init with Authenticode 542ff6b7…21cffc12 (release vcontainer-542ff6b77f72, commit 32c2cb47…)
  ✓ ledger: fresh boot key, our owner key
  state: Waiting

$ vcontainer deploy --session agent [REGISTRY]/openrouter-agent:v1 \
    -p 8080 --secret OPENROUTER_API_KEY=@openrouter.key
launching [REGISTRY]/openrouter-agent@sha256:acdf834c…
launch measured and committed; the VM is pulling the image
session agent ([IP], [ZONE]): VERIFIED
  ...
  ✓ launch measured before pull: [REGISTRY]/openrouter-agent@sha256:acdf834c…
  ✓ container sealed: manifest sha256:b38d15a0…
  state: Running
  container: [REGISTRY]/openrouter-agent@sha256:acdf834c… env=[] secrets={"OPENROUTER_API_KEY"}
  port 8080: http://[IP]:8080
```

On a `c3-standard-4`, `vcontainer run` takes about 35 s, and deploying the 43 MB demo image takes 20–30 s. The demo agent in [examples/openrouter-agent](examples/openrouter-agent) is 40 lines of Python. It answers chat requests through OpenRouter with a key read from `/run/secrets`.

### vcontainer run

```sh
vcontainer run --session NAME [--zone ZONE] [--machine-type TYPE] [--allow-from CIDR]
```

Creates a TDX VM, or reconnects to an existing one, and verifies it. The defaults are `us-central1-a` and `c3-standard-4`. Port 7700 (the attestation API) and any ports you later open with `-p` are reachable only from `--allow-from`, which defaults to your machine's public IP.

If the VM has rebooted since the last deploy (for example after host maintenance), `vcontainer run` reconnects to it empty; deploy again afterwards.

### vcontainer deploy

```sh
vcontainer deploy --session NAME IMAGE [options] [-- CMD ARGS...]
```

Verifies the session, sends the launch, verifies that the VM recorded it, and then lets the VM pull and start the image. `vcontainer pull` is an alias.

- `-p PORT` opens a container port to the `--allow-from` range.
- `-e KEY=VALUE` sets an environment variable. Variables are public (see [Trust](#trust)).
- `--secret NAME=@FILE`, `--secret NAME=VALUE` or `--secret NAME` (taken from your environment) seals a secret to the VM. It appears as `/run/secrets/NAME`.
- `--user`, `--workdir` and `--entrypoint` override the image's settings.
- `--quiet` keeps container output off the serial console.
- `--resume` finishes a deploy that was interrupted after its launch was sent but before the commit: it commits that launch and opens its saved ports.

Images must be linux/amd64 and fit in RAM. The kernel is minimal; for example, it has no user namespaces or io_uring. A tag is resolved to a digest before launch, and the digest is what gets measured. Private images work on Artifact Registry and gcr.io, where your `gcloud` access token is sealed to the VM.

### vcontainer verify

```sh
vcontainer verify --session NAME [--export-policy FILE]
vcontainer verify --url http://[IP]:7700 --policy FILE
```

Verifies a session again at any time. `--export-policy` writes everything a third party needs: the expected measurements, your public key, the instance and the exact launch. With that file and network access to port 7700, they run the second form. They need neither your key nor your cloud account.

### vcontainer logs, ls, rm

```sh
vcontainer logs --session NAME     # the serial console: kernel, /init and container output
vcontainer ls                      # your sessions and what each one runs
vcontainer rm --session NAME       # delete the VM and its firewall rule
```

A session bills as a running VM until you remove it.

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
- traffic to the container's ports, which vContainer does not encrypt. The demo agent serves plain HTTP. To protect this traffic, have the container serve TLS with a key passed through `--secret`, and publish the key's fingerprint with `-e` so clients can check it against the verified evidence.

## Status

Version 0.1 works end to end on real hardware. The release built from commit 32c2cb4 was rebuilt from scratch, also in a fresh clone, and matched byte for byte.

Intel TDX on Google Cloud C3 is a demo platform, not part of the design. The design needs a measured boot, an append-only measurement register and quotes that bind data chosen by the guest. Supporting another platform means replacing the TDX and Google Cloud parts of `/init`, the verifier, the CLI's cloud commands, the kernel configuration and the image build.

The code had three Codex review rounds and one adversarial multi-agent review, and every confirmed finding was fixed. A later audit replaced the fixes that had piled up over those rounds with one mechanism each. [Whitepaper §8](docs/whitepaper.md#8-security-analysis) summarizes both.

Limits:

- One container runs per session, and its root filesystem lives in RAM.
- After host maintenance the VM reboots empty. Run `vcontainer run`, then `vcontainer deploy`, again. Until you do, `vcontainer verify` reports that the container was not redeployed.
- There is no SEV-SNP support. On Google Cloud's generally available SNP machines, the kernel can be identified only through a vTPM that the host runs. [docs/design](docs/design/) has the analysis.

## Learn more

- [docs/whitepaper.md](docs/whitepaper.md) presents the design top-down: what a PASS establishes, what it does not, and why.
- [image/README-image.md](image/README-image.md) covers the kernel configuration and the platform facts measured on real C3 hardware.
- Code:
  - [crates/core](crates/core): the protocol and the OCI client;
  - [crates/init](crates/init): the guest `/init`;
  - [crates/cli](crates/cli): the CLI and verifier.
