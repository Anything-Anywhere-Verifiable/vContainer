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
| Build scripts | about 110 lines of shell |

The VM boots a single file: a Linux kernel with a built-in initramfs whose only program is `/init`. There is no bootloader, root disk, systemd or container runtime. Until a container is deployed, the VM runs only the kernel and `/init`, which links musl and crates such as rustls, hpke and tar. The build is designed to be reproducible, and the file's measurement is predicted offline.

## How it works

The steps below are for the demo platform, Intel TDX on Google Cloud C3.

1. **Boot and measure.** `vcontainer run` creates the VM. Before anything else, `/init` records a fresh key for this boot, then your public key, in a hardware register that can only be extended.
2. **Verify the empty VM.** Your machine asks the VM for a quote over a fresh nonce and checks it: Intel's signature, Google's signed firmware, the exact boot image and the recorded keys.
3. **Launch, verify, commit.** `vcontainer deploy` sends a launch signed with your key: the image digest, the container settings, and the secrets, encrypted to the boot key. `/init` records the launch. Your machine checks that the record is the launch it sent, and only then lets the VM pull.
4. **Pull and run.** The VM pulls the image by digest, checks every layer and starts the container. Secrets appear as files in `/run/secrets`.

As a result, you have verified the VM's boot and its empty state before it pulls anything, and secrets reach only that VM. The container cannot extend the register or request quotes over its own data, so it cannot forge the evidence.

## Install

Requirements:

- `rustup` (the repository pins Rust 1.95.0). The CLI runs on Linux and on macOS, Apple Silicon or Intel; CI builds and tests it on both Macs.
- To build the boot image: Linux x86-64 with Docker. On a Mac, build and publish the image on a Linux machine, then copy `~/.config/vcontainer/release.json` to the Mac and install only the CLI.
- A Google Cloud project with the Compute Engine API enabled and C3 quota in your zone.
- `gcloud`, logged in.

Each block below is one command, so you can copy and run them one at a time.

Set your Google Cloud project (replace `PROJECT` with its ID):

```sh
gcloud config set project PROJECT
```

Get the code:

```sh
git clone https://github.com/Mengyuan-L/vContainer && cd vContainer
```

Build `/init` and the kernel in a pinned build container. The result is `out/BOOTX64.EFI` and `out/disk.raw`:

```sh
scripts/build-image.sh
```

Install the `vcontainer` CLI:

```sh
cargo install --locked --path crates/cli
```

Create the GCE image in your project and record its expected measurements:

```sh
vcontainer publish
```

You build and publish the boot image once. It is the same verified image for every container you run: the container image is pulled and measured only when you deploy it, so a new or changed agent image never needs a new boot image. Rebuild only when vContainer itself changes. The first build downloads the kernel source and the build container's packages.

`scripts/build-image.sh` refuses a checkout with uncommitted changes; pass `--dev` while hacking. Anyone can rebuild a release byte for byte from its commit, which `vcontainer publish` records and `vcontainer verify` prints. To see the expected measurements without publishing, run `vcontainer measure`.

Your owner key and session files live in `~/.config/vcontainer`. The key is created on first use.

## Usage

A session is one VM, and each boot of it runs one container. You start a session, deploy one container into it, and remove the session when you are done. The two examples below each follow that order.

### Example 1: a public image

Start a session named `demo`. The CLI creates the VM and verifies it before anything is sent to it:

```sh
vcontainer run --session demo
```

The output lists every check. `[IP]`, `[ZONE]` and `[ID]` stand for your session's values:

```console
creating TDX confidential VM demo (c3-standard-4 in [ZONE])
waiting for http://[IP]:7700 to boot and answer attestation
session demo ([IP], [ZONE]): VERIFIED
  ✓ Intel TDX quote: signature valid, TCB UpToDate (evaluation 20)
  ✓ fresh quote for our nonce; state Waiting
  ✓ TD attributes: debug off (0x10000000), XFAM 0x602e7
  ✓ GCE instance [ID] in [ZONE] (MROWNER)
  ✓ firmware: MRTD c1ee9c16e3afc506… endorsed by Google (svn 2)
  ✓ host: a Google Cloud machine (PPID in Google's host registry)
  ✓ boot image: kernel+initramfs+cmdline+/init with Authenticode 24e25d82…3dc9fd7d (release vcontainer-24e25d8234a7, commit c2f795b3…)
  ✓ ledger: fresh boot key, our owner key
  state: Waiting
```

This takes about 35 seconds. Deploy Docker Hub's `hello-world` image into the verified VM. Images are always named in full, with the registry host and a tag or digest:

```sh
vcontainer deploy --session demo docker.io/library/hello-world:latest
```

The CLI resolves the tag to a digest, sends the launch, checks that the VM recorded it, and only then lets the VM pull. The container prints its message and exits, so the session ends in state `Exited (exit code 0)`. Read the message from the serial console:

```sh
vcontainer logs --session demo
```

Remove the session. The VM bills until you do, even after its container exits:

```sh
vcontainer rm --session demo
```

### Example 2: your own agent image, with an API key

The [OpenRouter agent](examples/openrouter-agent) is 40 lines of Python. It answers `POST /chat` through OpenRouter with an API key that only the verified VM can read. You build its image yourself, push it to a private Artifact Registry repository, and pass the key as a secret.

Set `REGISTRY` to your repository; `docker push` sends the image to whatever host and repository its name starts with. It is an ordinary shell variable that only shortens the commands below: `vcontainer` never reads it, and it lasts only for this terminal. Replace `PROJECT` with your project ID; `vcontainer` is the repository name used here:

```sh
REGISTRY=us-central1-docker.pkg.dev/PROJECT/vcontainer
```

Create the repository (once per project):

```sh
gcloud artifacts repositories create vcontainer --repository-format=docker --location=us-central1
```

Let Docker push to it (once per machine):

```sh
gcloud auth configure-docker us-central1-docker.pkg.dev
```

Build the agent image and push it:

```sh
docker build --platform linux/amd64 -t "$REGISTRY/openrouter-agent:v1" examples/openrouter-agent
```

```sh
docker push "$REGISTRY/openrouter-agent:v1"
```

Save your OpenRouter API key in a file named `openrouter.key` that holds only the key. Then start a session:

```sh
vcontainer run --session agent
```

Deploy the agent. `--secret` encrypts the key to this VM, and `-p 8080` opens the agent's port to your IP:

```sh
vcontainer deploy --session agent "$REGISTRY/openrouter-agent:v1" -p 8080 --secret OPENROUTER_API_KEY=@openrouter.key
```

```console
launching us-central1-docker.pkg.dev/[PROJECT]/vcontainer/openrouter-agent@sha256:acdf834c…
launch measured and committed; the VM is pulling the image
session agent ([IP], [ZONE]): VERIFIED
  ...
  ✓ launch measured before pull: us-central1-docker.pkg.dev/[PROJECT]/vcontainer/openrouter-agent@sha256:acdf834c…
  ✓ container sealed: manifest sha256:b38d15a0…
  state: Running
  container: us-central1-docker.pkg.dev/[PROJECT]/vcontainer/openrouter-agent@sha256:acdf834c… env=[] secrets={"OPENROUTER_API_KEY"}
  port 8080: http://[IP]:8080
```

Deploying this 43 MB image takes 20–30 seconds. The agent reads its key from `/run/secrets/OPENROUTER_API_KEY`. Set `IP` to the address in the last line (the one below is only an example), then chat with the agent:

```sh
IP=203.0.113.7
```

```sh
curl -X POST "http://$IP:8080/chat" -d '{"message":"Reply with exactly: vContainer works"}'
```

Remove the session when you are done:

```sh
vcontainer rm --session agent
```

### Command reference

**`vcontainer run --session NAME`** creates a VM, or reconnects to an existing one, and verifies it. Options:

- `--zone ZONE` and `--machine-type TYPE` choose where and what to run (defaults: `us-central1-a`, `c3-standard-4`).
- `--allow-from CIDR` sets who can reach the attestation port (7700) and the container's ports (default: your public IP).

**`vcontainer deploy --session NAME IMAGE`** verifies the session, sends the launch, verifies that the VM recorded it, and then lets the VM pull and start the image. `vcontainer pull` is an alias. Options:

- `-p PORT` opens a container port to the `--allow-from` range.
- `-e KEY=VALUE` sets an environment variable. Variables are public (see [Trust](#trust)).
- `--secret NAME=@FILE`, `--secret NAME=VALUE` or `--secret NAME` (taken from your environment) encrypts a secret to the VM. It appears as `/run/secrets/NAME`.
- `--user`, `--workdir` and `--entrypoint` override the image's settings, and `-- CMD ARGS...` at the end overrides its command.
- `--quiet` keeps container output off the serial console.
- `--resume` finishes a deploy that was interrupted after its launch was sent but before the commit.

`IMAGE` is a full reference, `HOST/REPOSITORY:TAG` or `HOST/REPOSITORY@DIGEST`; nothing is filled in for you. Images must be linux/amd64 and fit in RAM. The kernel is minimal; for example, it has no user namespaces or io_uring. A tag is resolved to a digest before launch, and the digest is what gets measured. Public images are pulled without credentials (Docker Hub is tested); private images work on Artifact Registry and gcr.io, where your `gcloud` access token is sealed to the VM.

**`vcontainer verify --session NAME`** verifies a session again at any time. `--export-policy FILE` also writes what a third party needs: the expected measurements, your public key, the instance and the exact launch, but no secrets:

```sh
vcontainer verify --session agent --export-policy agent-policy.json
```

With that file and the session's address in `IP`, anyone who can reach port 7700 verifies the session, without your key or your cloud account:

```sh
vcontainer verify --url "http://$IP:7700" --policy agent-policy.json
```

**`vcontainer logs --session NAME`** prints the serial console: kernel, `/init` and container output. **`vcontainer ls`** lists your sessions and what each one runs. **`vcontainer rm --session NAME`** deletes the VM and its firewall rule.

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

Version 0.1 works end to end on real hardware. The release built from commit c2f795b was rebuilt from scratch, also in a fresh clone, and matched byte for byte.

Intel TDX on Google Cloud C3 is a demo platform, not part of the design. The design needs a measured boot, an append-only measurement register and quotes that bind data chosen by the guest. Supporting another platform means replacing the TDX and Google Cloud parts of `/init`, the verifier, the CLI's cloud commands, the kernel configuration and the image build.

The code had three Codex review rounds and one adversarial multi-agent review, and every confirmed finding was fixed. A later audit replaced the fixes that had piled up over those rounds with one mechanism each. [Whitepaper §8](docs/whitepaper.md#8-security-analysis) summarizes both.

Limits:

- One container per VM boot, with its root filesystem in RAM.
- After host maintenance the VM reboots empty: run `vcontainer run`, then `vcontainer deploy`, again.
- There is no SEV-SNP support. On Google Cloud's generally available SNP machines, the kernel can be identified only through a vTPM that the host runs. [docs/design](docs/design/) has the analysis.

## Learn more

- [docs/whitepaper.md](docs/whitepaper.md) presents the design top-down: what a PASS establishes, what it does not, and why.
- [image/README-image.md](image/README-image.md) covers the kernel configuration and the platform facts measured on real C3 hardware.
- Code:
  - [crates/core](crates/core): the protocol and the OCI client;
  - [crates/init](crates/init): the guest `/init`;
  - [crates/cli](crates/cli): the CLI and verifier.
