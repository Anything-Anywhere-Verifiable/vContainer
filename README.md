# vContainer

Deploy your agent in a confidential VM in the cloud, keeping its memory and secrets private from the cloud host.

vContainer manages deployment and verification for your containerized agent. You bring the container image and API keys; vContainer creates the VM, verifies what booted, delivers your keys encrypted to that VM, and starts the agent only after verifying its recorded launch. The current implementation runs on Intel TDX confidential VMs on Google Cloud C3.

- **Deploy:** run an ordinary `linux/amd64` agent container in a confidential VM.
- **Verify:** check hardware evidence of the VM's boot image and the exact agent launch.
- **Deliver secrets:** encrypt API keys to the verified VM and expose them to your agent as files.
- **Share verification:** give your users a policy file so they can check the same evidence.

<<<<<<< HEAD
| Deployment guide | Start with |
|---|---|
| [Deploy a container](docs/deploy-a-container.md) | A public hello-world image or another container you already have |
| [Deploy an OpenRouter agent](docs/deploy-an-openrouter-agent.md) | The included Python agent and your OpenRouter API key |
| [Deploy your own agent](docs/deploy-your-agent.md) | Your agent code and a Dockerfile |
=======
**Contents:** [How it works](#how-it-works) · [Quick start](#quick-start) · [Install](#install) · [More examples](#more-examples) · [Command reference](#command-reference) · [Trust](#trust) · [Status](#status)

## How it works

vContainer keeps the code you have to trust small:

| Part | Size |
|---|---|
| Code we wrote that runs in the VM (`/init` and the protocol) | about 1,450 lines of Rust |
| CLI and verifier, on your machine | about 1,100 lines of Rust |
| Kernel configuration | `allnoconfig` plus 107 options |
| Build scripts | about 110 lines of shell |
>>>>>>> 89b711f6842958daf4572c1ad3f62d4328de7694

To try it first, [install vContainer](#install) and [run hello-world](#example-1-a-public-image). For the privacy boundary, including application traffic and logs, see [Trust](#trust).

The steps below are for the demo platform, Intel TDX on Google Cloud C3.

```mermaid
sequenceDiagram
    actor Owner
    participant CLI as vcontainer CLI (your machine)
    participant VM as Confidential VM (kernel + /init)
    participant Registry as Container registry
    Owner->>CLI: run --session demo
    CLI->>VM: Create VM from published boot image
    VM-->>CLI: Fresh hardware quote, boot measurements, boot key
    CLI->>CLI: Verify firmware, boot image, and empty state
    Owner->>CLI: deploy IMAGE --secret NAME=@FILE
    CLI->>VM: Signed launch with image digest and encrypted secrets
    VM->>VM: Record launch in hardware-backed ledger
    VM-->>CLI: Fresh evidence of recorded launch
    CLI->>CLI: Check launch matches what was sent
    CLI->>VM: Signed commit authorizing image pull
    VM->>Registry: Pull image by digest and verify layers
    VM->>VM: Run container with secrets in /run/secrets
```

1. **Boot and measure.** `vcontainer run` creates the VM. Before anything else, `/init` records a fresh key for this boot, then your public key, in a hardware register that can only be extended.
2. **Verify the empty VM.** Your machine asks the VM for a quote over a fresh nonce and checks it: Intel's signature, Google's signed firmware, the exact boot image and the recorded keys.
3. **Launch, verify, commit.** `vcontainer deploy` sends a launch signed with your key: the image digest, the container settings, and the secrets, encrypted to the boot key. `/init` records the launch. Your machine checks that the record is the launch it sent, and only then lets the VM pull.
4. **Pull and run.** The VM pulls the image by digest, checks every layer and starts the container. Secrets appear as files in `/run/secrets`.

As a result, you have verified the VM's boot and its empty state before it pulls anything, and secrets reach only that VM. The container cannot extend the register or request quotes over its own data, so it cannot forge the evidence.

<<<<<<< HEAD
The VM boots a single file: a Linux kernel with a built-in initramfs whose only program is `/init`. There is no bootloader, root disk, systemd or container runtime. Until a container is deployed, the VM runs only the kernel and `/init`. The build is designed to be reproducible, and the file's measurement is predicted offline.

## Install

### Prerequisites

- Python 3.9+ for the installer; it uses only the standard library.
- A native C linker/toolchain for the CLI. The installer runs `rustup` to prepare the repository's Rust toolchain (pinned to 1.95.0). If rustup is missing, it downloads and runs the official installer; this requires `curl` and `sh`.
- To build the boot image: **Linux x86-64**, Git, Bash, and a running Docker daemon accessible without `sudo`. Kernel and disk-building tools run inside the pinned Docker build container.
- To publish or run VMs: `gcloud`, logged in with `gcloud auth login`, and a Google Cloud project with Compute Engine enabled. You need permission to create Cloud Storage buckets/objects and Compute Engine images; running sessions also needs VM/network/firewall permissions and C3 quota in your zone.

The installer sets up Rust automatically and checks the other prerequisites; install those tools yourself before running it. It reuses rustup when available, leaves your global Rust default and shell startup files unchanged, and makes Cargo available immediately for the remaining setup steps. The first build downloads Rust dependencies, the kernel source, and the build container's packages.

### Build, install, and publish on Linux x86-64

Get the code:

```sh
git clone https://github.com/Mengyuan-L/vContainer && cd vContainer
```

Preview the setup commands without executing anything:

```sh
python3 scripts/install.py --publish --project PROJECT --dry-run
```

Replace `PROJECT` with your project ID and run setup:

```sh
python3 scripts/install.py --publish --project PROJECT
```

This builds `/init`, the kernel, and the boot disk through `scripts/build-image.sh`; installs the Rust CLI; checks its help and expected measurements; then publishes the image to Google Cloud. Publishing creates cloud resources (a storage bucket if needed, an uploaded disk, and a Compute Engine image); it does not start a VM. The installer stops on the first failed command. It can be invoked from any working directory.

The `vcontainer` executable normally goes in `~/.cargo/bin`. Add that directory to your shell's `PATH` if needed:

```sh
export PATH="$HOME/.cargo/bin:$PATH"
```

Set the project for subsequent CLI commands; the installer's `--project` applies only to that setup invocation:

```sh
gcloud config set project PROJECT
```

Then continue with [Example 1](#example-1-a-public-image).

| Setup option | What it does |
|---|---|
| No flags | Builds the boot image and installs the CLI locally; does not access Google Cloud. Publish later with `vcontainer publish`. |
| `--publish --project PROJECT` | Also publishes the boot image and saves the local release settings. |
| `--cli-only` | Installs only the CLI; useful on macOS or when the boot image is already published. |
| `--dev` | Allows local source changes; the resulting image is not recorded as a release from a clean commit. |
| `--clean` | Drops cached kernel source/objects before building; clean release builds already do this. |
| `--install-root PATH` | Chooses the Cargo installation root; the executable goes under `PATH/bin`. Defaults to `CARGO_INSTALL_ROOT`, then `CARGO_HOME`, then `~/.cargo`. |
| `--dry-run` | Prints commands without executing them or checking prerequisites. |

Use `python3 scripts/install.py --help` for all options. The installer covers normal setup; the hardware diagnostic probe scripts are a separate workflow in [image/README-image.md](image/README-image.md).

### CLI only on macOS or another Linux machine

The CLI runs on Linux and macOS (Apple Silicon or Intel). In a checkout on the client machine:

```sh
python3 scripts/install.py --cli-only
```

Build and publish the boot image on a Linux x86-64 machine first. Copy its `~/.config/vcontainer/release.json` to that same path on your client, authenticate `gcloud`, and set the same project. Each client creates its own owner key on first use; you do not need to copy the builder's private key.

### What gets built and saved

There are two different images:

| Image | Purpose | When to rebuild |
|---|---|---|
| **Boot image**, built by this repository | Kernel + `/init` that records and launches your workload | When vContainer itself changes |
| **Application/container image**, supplied by you or a registry | Your agent or other application, as a `linux/amd64` OCI image | When your application changes; no boot-image rebuild needed |

Local boot artifacts are in `out/`: `BOOTX64.EFI`, `disk.raw`, `vcontainer.tar.gz`, measurements, and build metadata. Publishing writes `~/.config/vcontainer/release.json`, which tells the CLI which cloud image and measurements to expect. Your owner key and session files also live in `~/.config/vcontainer`.

<details>
<summary>Equivalent manual setup commands (from the repository root)</summary>

```sh
scripts/build-image.sh
cargo install --locked --path crates/cli
gcloud config set project PROJECT
vcontainer publish
```

</details>

Release builds require a clean Git checkout. While changing code or documentation, use `python3 scripts/install.py --dev` (or `scripts/build-image.sh --dev` for a manual build). Anyone can rebuild a release byte for byte from its commit, which `vcontainer publish` records and `vcontainer verify` prints. To inspect measurements later without publishing, run `vcontainer measure`.

## Usage

A session is one VM, and each boot of it runs one container. You start a session, deploy one container into it, and remove the session when you are done. Try the examples below, or use the deployment guides above for a full walkthrough from prerequisites to cleanup.

### Example 1: a public image
=======
## Quick start

This example runs Docker Hub's `hello-world` image in a verified confidential VM on Google Cloud.

**[Install](#install) first.** You need the `vcontainer` CLI and a boot image published to your Google Cloud project. The Install section gives a command for every step, from installing `rustup` to `vcontainer publish`.

A session is one VM, and each boot of it runs one container. You start a session, deploy one container into it, and remove the session when you are done. Every example in this README follows that order.
>>>>>>> 89b711f6842958daf4572c1ad3f62d4328de7694

For setup and more container options, follow [Deploy a container](docs/deploy-a-container.md).

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

```shx^
vcontainer rm --session demo
```

Next, [run an agent that reviews a pull request](#more-examples).

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

The examples deploy a published image, so they need no Docker. To build your own container images on a Mac, as in the review agent's [Build your own image](examples/pr-review-agent/README.md#build-your-own-image), you also need Docker Desktop.

## More examples

### An agent with tools: review a pull request

The [pull request review agent](examples/pr-review-agent) works through tools. It reviews one GitHub pull request at one commit: the model reads the diff and other text files of the repository, including unchanged ones, through five tools, with no shell, and finishes with a review. Its image is on Docker Hub, so nothing needs to be built. Here it reviews [pull request #1](https://github.com/haoxxu/vContainer-review-demo/pull/1) of a small demo repository, which adds a function with three deliberate bugs.

Save your OpenRouter API key in a file named `openrouter.key` that holds only the key. Then start a session:

```sh
vcontainer run --session public-review
```

Deploy the agent, pinned by digest. The pull request and its commit are environment variables, so they are part of the measured launch; if the pull request has moved to another commit, the agent stops without reviewing it. The agent opens no port:

```sh
vcontainer deploy --session public-review docker.io/terryxhx/pr-review-agent@sha256:fc20c8d1369edacbd77405847d94efdca733df17b883a908d0db29e28865776b -e PR_URL=https://github.com/haoxxu/vContainer-review-demo/pull/1 -e HEAD_SHA=688c61413e4e9bf830a58bab3e344d25582b8a33 --secret OPENROUTER_API_KEY=@openrouter.key
```

The deploy returns once the container runs, after about 15 seconds. The review takes a minute or two; then verify the session again:

```sh
vcontainer verify --session public-review
```

```console
public-review: VERIFIED
  ...
  ✓ launch measured before pull: docker.io/terryxhx/pr-review-agent@sha256:fc20c8d1…
  ✓ container sealed: manifest sha256:fc20c8d1…
  state: Exited (exit code 0)
  container: docker.io/terryxhx/pr-review-agent@sha256:fc20c8d1… env=["PR_URL=https://github.com/haoxxu/vContainer-review-demo/pull/1", "HEAD_SHA=688c61413e4e9bf830a58bab3e344d25582b8a33"] secrets={"OPENROUTER_API_KEY"}
```

The evidence names the agent's image, the pull request and the commit, and the exit code is part of the quote. Read the review from the serial console; `sed` skips the boot messages before it:

```sh
vcontainer logs --session public-review | sed -n '/^pr-review-agent:/,$p'
```

```console
pr-review-agent: haoxxu/vContainer-review-demo#1 at 688c61413e4e, model anthropic/claude-sonnet-5.5
  tool get_diff {"path": "notes/listing.py"}
  tool list_dir {"path": "notes"}
  tool read_file {"path": "notes/cache.py"}
  tool read_file {"path": "notes/db.py"}
  tool get_diff {"path": "notes/__init__.py"}

review of haoxxu/vContainer-review-demo#1 at 688c61413e4e: 4 comments

The new `list_notes` has four defects. Two of them make it wrong on every call, and one is a SQL injection. I read the diff and `notes/cache.py` and `notes/db.py`. I did not run the code.

notes/listing.py:16
  `Cache.get` raises KeyError on a miss or an expired entry. It never returns None, as its docstring in cache.py says. So the first call for any key raises KeyError and `list_notes` never reaches the database. Wrap the lookup in try/except KeyError and fall through to the query.
...
tokens: 9651 prompt, 1067 completion
vcontainer: container exited with 0
```

In this run the agent found the three planted bugs and further problems with the cache. It found the cache bug by reading `notes/cache.py`, a file the pull request does not change. The review varies from run to run.

The OpenRouter key is sealed to the VM, and the model never sees it: the agent calls OpenRouter itself and gives the model only its tools. Everything else here is public: the repository, the launch and the review on the serial console. OpenRouter and the model's provider see the code under review. Remove the session when you are done:

```sh
vcontainer rm --session public-review
```

### Review a private pull request

The same agent can review a pull request in a private repository and post the review there, while the host sees neither the code the agent reads, the token nor the review. You copy the demo repository into a private one, give the agent a GitHub token for that repository only, and keep the container's output off the serial console. The demo's code is public anyway; with your own private repository, the same steps keep its code from the host.

This example also uses the [GitHub CLI](https://cli.github.com), `gh`, signed in with `gh auth login` to the account that will own the copy.

Copy the demo repository, with its history, into a new private repository named `review-demo`. "Use this template" does not work here: the branches of a template copy have unrelated histories, so GitHub cannot open a pull request between them. Create the empty repository:

```sh
gh repo create review-demo --private
```

Let `git` push to GitHub over HTTPS with your `gh` login (once per machine). If you push over SSH instead, skip this and push to `git@github.com:OWNER/review-demo.git` below:

```sh
gh auth setup-git
```

Clone the demo repository without a working tree:

```sh
git clone --bare https://github.com/haoxxu/vContainer-review-demo.git
```

Push every branch to your copy. Replace `OWNER` with your GitHub account, here and below:

```sh
git -C vContainer-review-demo.git push --mirror https://github.com/OWNER/review-demo.git
```

Open the pull request in your copy:

```sh
gh pr create --repo OWNER/review-demo --base main --head feat/list-notes --title "feat: add paginated note listing" --body "Adds list_notes, which lists the notes of one owner, a page at a time."
```

Create a [fine-grained personal access token](https://github.com/settings/personal-access-tokens/new) for this repository only. Under **Repository access**, choose **Only select repositories** and `review-demo`. Under **Permissions**, give **Contents** read-only and **Pull requests** read and write. Save the token in a file named `github.key` that holds only the token. Then start a session:

```sh
vcontainer run --session private-review
```

Deploy the same image as in the previous example. The copy keeps the demo's commits, so `HEAD_SHA` does not change. With `GITHUB_TOKEN`, the agent posts its review to the pull request and prints nothing of the repository, and `--quiet` keeps even that off the serial console:

```sh
vcontainer deploy --session private-review docker.io/terryxhx/pr-review-agent@sha256:fc20c8d1369edacbd77405847d94efdca733df17b883a908d0db29e28865776b -e PR_URL=https://github.com/OWNER/review-demo/pull/1 -e HEAD_SHA=688c61413e4e9bf830a58bab3e344d25582b8a33 --secret OPENROUTER_API_KEY=@openrouter.key --secret GITHUB_TOKEN=@github.key --quiet
```

After a minute or two, verify the session:

```sh
vcontainer verify --session private-review
```

```console
private-review: VERIFIED
  ...
  state: Exited (exit code 0)
  container: docker.io/terryxhx/pr-review-agent@sha256:fc20c8d1… env=["PR_URL=https://github.com/OWNER/review-demo/pull/1", "HEAD_SHA=688c61413e4e9bf830a58bab3e344d25582b8a33"] secrets={"GITHUB_TOKEN", "OPENROUTER_API_KEY"}
```

Both secret names are part of the evidence, so whoever checks the session can see that the agent had a GitHub token. The serial console shows the container start and exit, and nothing in between:

```sh
vcontainer logs --session private-review | tail -n 2
```

```console
vcontainer: container started: python3 -u /app/agent.py
vcontainer: container exited with 0
```

Open the pull request to read the review. It is a comment review of commit `688c6141`, posted by the account that owns the token, with inline comments on the lines it discusses:

```sh
gh pr view 1 --repo OWNER/review-demo --web
```

What this run keeps from whom:

- The host cannot read the code the agent reads, the token or the review. The token is sealed to the VM, the agent keeps the code in the VM's encrypted memory, its traffic to GitHub and OpenRouter is HTTPS, the container's output stays off the serial console, and the VM has no SSH or exec.
- The launch is public: the image, the commit and `PR_URL`, which names your repository.
- GitHub, OpenRouter and the model's provider see the code. So does anything your GitHub settings let read the repository, such as an app or automatic Copilot code review; vContainer does not change those settings. The host sees which servers the VM connects to and how much data it exchanges with them.
- The image is the one we publish, pinned by digest. `vcontainer verify` shows which image ran, but not that it was built from [examples/pr-review-agent](examples/pr-review-agent), so you trust its publisher. To run your own build, follow [Build your own image](examples/pr-review-agent/README.md#build-your-own-image) and deploy that reference instead.

Remove the session when you are done. The token and the private repository stay until you delete them on GitHub:

```sh
vcontainer rm --session private-review
```

## Command reference

**`vcontainer run --session NAME`** creates a VM, or reconnects to an existing one, and verifies it. Options:

- `--zone ZONE` and `--machine-type TYPE` choose where and what to run (defaults: `us-central1-a`, `c3-standard-4`).
- `--allow-from CIDR` sets who can reach the attestation port (7700) and the container's ports (default: your public IP).

`--allow-from` and published ports control incoming connections only. vContainer does not implement outbound destination filtering or an egress allowlist.

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
vcontainer verify --session public-review --export-policy review-policy.json
```

With that file and the session's address in `IP`, anyone who can reach port 7700 verifies the session, without your key or your cloud account:

```sh
vcontainer verify --url "http://$IP:7700" --policy review-policy.json
```

**`vcontainer logs --session NAME`** prints the serial console: kernel, `/init` and container output. **`vcontainer ls`** lists your sessions and what each one runs. **`vcontainer rm --session NAME`** deletes the VM and its firewall rule.

## Trust

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

## Status

Version 0.1 works end to end on real hardware. The release built from commit c2f795b was rebuilt from scratch, also in a fresh clone, and matched byte for byte.

Intel TDX on Google Cloud C3 is a demo platform, not part of the design. The design needs a measured boot, an append-only measurement register and quotes that bind data chosen by the guest. Supporting another platform means replacing the TDX and Google Cloud parts of `/init`, the verifier, the CLI's cloud commands, the kernel configuration and the image build.

The code had three Codex review rounds and one adversarial multi-agent review, and every confirmed finding was fixed. A later audit replaced the fixes that had piled up over those rounds with one mechanism each. [Whitepaper §8](docs/whitepaper.md#8-security-analysis) summarizes both.

Limits:

- One container per VM boot, with its root filesystem in RAM.
- After host maintenance the VM reboots empty: run `vcontainer run`, then `vcontainer deploy`, again.
- There is no SEV-SNP support. On Google Cloud's generally available SNP machines, the kernel can be identified only through a vTPM that the host runs. [docs/design](docs/design/) has the analysis.

## Learn more

- [Deploy a container](docs/deploy-a-container.md) starts with a public image and covers configuration, logs, verification, and cleanup.
- [Deploy an OpenRouter agent](docs/deploy-an-openrouter-agent.md) deploys the included demo with an encrypted API key.
- [Deploy your own agent](docs/deploy-your-agent.md) covers prerequisites, building your image, secret delivery, deployment, verification, and cleanup.
- [docs/whitepaper.md](docs/whitepaper.md) presents the design top-down: what a PASS establishes, what it does not, and why.
- [image/README-image.md](image/README-image.md) covers the kernel configuration and the platform facts measured on real C3 hardware.
- [examples/openrouter-agent/README.md](examples/openrouter-agent/README.md) walks through building and deploying your own agent image.
- [scripts/install.py](scripts/install.py) orchestrates normal build/install/publish setup; [scripts/build-image.sh](scripts/build-image.sh) contains the reproducible boot-image build.
- Code:
  - [crates/core](crates/core): the protocol and the OCI client;
  - [crates/init](crates/init): the guest `/init`;
  - [crates/cli](crates/cli): the CLI and verifier.
