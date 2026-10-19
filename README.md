# vContainer

vContainer is a minimal implementation for running an AI agent in the cloud confidentially and verifiably. It runs the agent, or any other container, in a confidential VM that you can verify before you trust it.

An agent in the cloud holds API keys and often handles private data. On an ordinary VM, the cloud host can read both. A confidential VM keeps its memory encrypted from the host, and the hardware signs a quote of what the VM booted. vContainer builds on this:

- you verify the VM's firmware, boot image and empty state before the agent's image or keys reach it;
- the agent's API keys are encrypted so that only the verified VM can read them;
- others, such as the agent's users, can check the same evidence with a policy file you give them.

**Contents:** [How it works](#how-it-works) · [Quick start](#quick-start) · [Install](#install) · [More examples](#more-examples) · [Command reference](#command-reference) · [Trust](#trust) · [Status](#status)

## How it works

vContainer keeps the code you have to trust small:

| Part | Size |
|---|---|
| Code we wrote that runs in the VM (`/init` and the protocol) | about 1,450 lines of Rust |
| CLI and verifier, on your machine | about 950 lines of Rust |
| Kernel configuration | `allnoconfig` plus 107 options |
| Build scripts | about 110 lines of shell |

The VM boots a single file: a Linux kernel with a built-in initramfs whose only program is `/init`. There is no bootloader, root disk, systemd or container runtime. Until a container is deployed, the VM runs only the kernel and `/init`, which links musl and crates such as rustls, hpke and tar. The build is designed to be reproducible, and the file's measurement is predicted offline.

The steps below are for the demo platform, Intel TDX on Google Cloud C3.

1. **Boot and measure.** `vcontainer run` creates the VM. Before anything else, `/init` records a fresh key for this boot, then your public key, in a hardware register that can only be extended.
2. **Verify the empty VM.** Your machine asks the VM for a quote over a fresh nonce and checks it: Intel's signature, Google's signed firmware, the exact boot image and the recorded keys.
3. **Launch, verify, commit.** `vcontainer deploy` sends a launch signed with your key: the image digest, the container settings, and the secrets, encrypted to the boot key. `/init` records the launch. Your machine checks that the record is the launch it sent, and only then lets the VM pull.
4. **Pull and run.** The VM pulls the image by digest, checks every layer and starts the container. Secrets appear as files in `/run/secrets`.

As a result, you have verified the VM's boot and its empty state before it pulls anything, and secrets reach only that VM. The container cannot extend the register or request quotes over its own data, so it cannot forge the evidence.

## Quick start

This example runs Docker Hub's `hello-world` image in a verified confidential VM on Google Cloud.

**[Install](#install) first.** You need the `vcontainer` CLI and a boot image published to your Google Cloud project. The Install section gives a command for every step, from installing `rustup` to `vcontainer publish`.

A session is one VM, and each boot of it runs one container. You start a session, deploy one container into it, and remove the session when you are done. Every example in this README follows that order.

Start a session named `demo`. The CLI creates the VM and verifies it before anything is sent to it:

```sh
vcontainer run --session demo
```

While the CLI verifies, it draws a padlock in text whose shackle drops and locks; other steps show a spinner. Each step then leaves one `✓` line with how long it took, and the result lists every check. `[IP]`, `[ZONE]` and `[ID]` stand for your session's values:

```console
✓ network and firewall ready (9.1s)
✓ created TDX confidential VM demo: c3-standard-4 in [ZONE] (10.7s)
✓ demo booted and passed verification (12.7s)
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

Next, [run an agent with an API key](#more-examples).

## Install

The full install needs Linux on x86-64, because the boot image is built there. The CLI also runs on macOS, Apple Silicon or Intel; CI builds and tests it on both Macs. To use a Mac, see [Using a Mac](#using-a-mac) at the end of this section.

Each block below is one command, so you can copy and run them one at a time.

### 1. Install the tools

| Tool | Used for |
|---|---|
| `rustup`, a C compiler and `git` | building the `vcontainer` CLI |
| Docker | building the boot image, and your own container images |
| `gcloud` | everything the CLI does in Google Cloud |

**A C compiler, `git` and `curl`.** Some of the CLI's cryptography crates include C code. On Debian or Ubuntu:

```sh
sudo apt update && sudo apt install -y build-essential ca-certificates curl git gnupg
```

On macOS, install the Xcode Command Line Tools, which include `clang` and `git`:

```sh
xcode-select --install
```

**Rust.** Install `rustup`. The repository pins Rust 1.95.0 in `rust-toolchain.toml`, and `rustup` downloads that version the first time you build:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
```

Load `cargo` into the current terminal. New terminals load it automatically:

```sh
. "$HOME/.cargo/env"
```

**Docker.** On Debian or Ubuntu:

```sh
sudo apt install -y docker.io docker-buildx
```

The build script runs Docker as your user, so add yourself to the `docker` group. Then log out and back in for the change to take effect:

```sh
sudo usermod -aG docker "$USER"
```

On other Linux distributions, follow Docker's [install guide](https://docs.docker.com/engine/install/).

**gcloud.** On Debian or Ubuntu, add Google's package signing key:

```sh
curl -fsSL https://packages.cloud.google.com/apt/doc/apt-key.gpg | sudo gpg --dearmor -o /usr/share/keyrings/cloud.google.gpg
```

Add Google's package repository:

```sh
echo "deb [signed-by=/usr/share/keyrings/cloud.google.gpg] https://packages.cloud.google.com/apt cloud-sdk main" | sudo tee /etc/apt/sources.list.d/google-cloud-sdk.list
```

Install the Google Cloud CLI:

```sh
sudo apt update && sudo apt install -y google-cloud-cli
```

On macOS, with [Homebrew](https://brew.sh):

```sh
brew install --cask gcloud-cli
```

On other systems, follow Google's [install guide](https://cloud.google.com/sdk/docs/install).

### 2. Set up Google Cloud

You need a Google Cloud project with billing enabled and quota for C3 machines in your zone. The default session is a `c3-standard-4` in `us-central1-a`, which uses 4 C3 CPUs.

Log in:

```sh
gcloud auth login
```

Set your project. Replace `PROJECT` with its ID:

```sh
gcloud config set project PROJECT
```

Enable the Compute Engine API:

```sh
gcloud services enable compute.googleapis.com
```

### 3. Build and publish vContainer

Get the code:

```sh
git clone https://github.com/Mengyuan-L/vContainer && cd vContainer
```

Install the `vcontainer` CLI:

```sh
cargo install --locked --path crates/cli
```

Build `/init` and the kernel in a pinned build container. The result is `out/BOOTX64.EFI` and `out/disk.raw`. The first build downloads the kernel source and the build container's packages:

```sh
scripts/build-image.sh
```

Create the GCE image in your project and record its expected measurements:

```sh
vcontainer publish
```

You are ready for the [Quick start](#quick-start).

You build and publish the boot image once. It is the same verified image for every container you run: the container image is pulled and measured only when you deploy it, so a new or changed agent image never needs a new boot image. Rebuild only when vContainer itself changes.

`scripts/build-image.sh` refuses a checkout with uncommitted changes; pass `--dev` while hacking. Anyone can rebuild a release byte for byte from its commit, which `vcontainer publish` records and `vcontainer verify` prints. To see the expected measurements without publishing, run `vcontainer measure`.

Your owner key and session files live in `~/.config/vcontainer`. The key is created on first use.

### Using a Mac

Build and publish the boot image on a Linux machine, as above. On the Mac, install the C compiler, Rust and `gcloud` from step 1, do step 2, then clone the code and install the CLI from step 3. Last, copy `~/.config/vcontainer/release.json` from the Linux machine to the same path on the Mac.

To build your own container images on a Mac, as in the [last example](#your-own-agent-image-with-an-api-key), you also need Docker Desktop.

## More examples

### A public agent with an API key

Docker's [docker-agent](https://docs.docker.com/ai/docker-agent/) image runs an LLM agent and serves it as an OpenAI-compatible chat API. Nothing needs to be built: the image comes from Docker Hub, and the deploy command below gives it a small agent configuration that uses an OpenRouter model.

Save your OpenRouter API key in a file named `openrouter.key` that holds only the key. Then make a random access token for your chat clients; the agent answers only requests that carry it:

```sh
openssl rand -hex 16 > chat.key
```

Start a session:

```sh
vcontainer run --session chat
```

Deploy the agent with both keys as secrets. The command after `--` writes the agent configuration, reads the two secrets from `/run/secrets`, and starts the chat server on port 8080:

```sh
vcontainer deploy --session chat docker.io/docker/docker-agent:latest -p 8080 --secret OPENROUTER_API_KEY=@openrouter.key --secret CHAT_TOKEN=@chat.key -e TELEMETRY_ENABLED=false --entrypoint sh -- -c 'printf "agents:\n  root:\n    model: openrouter/openai/gpt-4o-mini\n    instruction: You are a helpful assistant.\n" > /tmp/agent.yaml && OPENROUTER_API_KEY=$(cat /run/secrets/OPENROUTER_API_KEY) CHAT_TOKEN=$(cat /run/secrets/CHAT_TOKEN) exec /docker-agent serve chat /tmp/agent.yaml --listen 0.0.0.0:8080 --api-key-env CHAT_TOKEN'
```

The agent configuration and the command are part of the measured launch, so `vcontainer verify` shows them; the two keys are not. Set `IP` to the address that `vcontainer deploy` printed (the one below is only an example):

```sh
IP=203.0.113.7
```

Ask the agent a question with your access token. Without the token it answers `401`:

```sh
curl "http://$IP:8080/v1/chat/completions" -H "Authorization: Bearer $(cat chat.key)" -H "Content-Type: application/json" -d '{"model":"root","messages":[{"role":"user","content":"Reply with exactly: vContainer works"}]}'
```

The token keeps others from using your key, but the traffic is plain HTTP, so the host can read it (see [Trust](#trust)). Remove the session when you are done:

```sh
vcontainer rm --session chat
```

### Your own agent image, with an API key

The [OpenRouter agent](examples/openrouter-agent) is 40 lines of Python. It answers `POST /chat` through OpenRouter with an API key that only the verified VM can read. You build its image yourself, push it to a private Artifact Registry repository, and pass the key as a secret.

Set `REGISTRY` to your repository; `docker push` sends the image to whatever host and repository its name starts with. It is an ordinary shell variable that only shortens the commands below: `vcontainer` never reads it, and it lasts only for this terminal. Replace `PROJECT` with your project ID; `vcontainer` is the repository name used here:

```sh
REGISTRY=us-central1-docker.pkg.dev/PROJECT/vcontainer
```

Enable the Artifact Registry API (once per project):

```sh
gcloud services enable artifactregistry.googleapis.com
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
✓ session agent passed verification (1.1s)
✓ sent the signed launch (1.2s)
✓ the VM measured exactly this launch (1.1s)
✓ committed: the VM may pull the image (5.7s)
✓ container Running (15.0s)
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

## Command reference

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
- traffic to the container's ports, which vContainer does not encrypt. The agents in the examples serve plain HTTP. To protect this traffic, have the container serve TLS with a key passed through `--secret`, and publish the key's fingerprint with `-e` so clients can check it against the verified evidence.

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
