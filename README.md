# vContainer

vContainer runs **containers and AI agents in confidential VMs**, with **as little code as possible**. You verify the VM before you deploy a container or send it secrets, and you can give others a policy file to check the same evidence independently.

An agent in the cloud holds API keys and often handles private data. A confidential VM protects its memory from the host; vContainer lets you verify what that VM booted and which container it runs before trusting it with those keys and data.

| Part | Size |
| --- | --- |
| Guest `/init`, protocol and OCI client | about 1,450 lines of Rust |
| Local CLI and verifier | about 950 lines of Rust |
| Kernel configuration | `allnoconfig` plus 107 options |
| Image build script | about 90 lines of shell |

These counts cover this repository's own code, without blank lines, comments, tests, Linux or third-party crates.

The VM boots one measured file: a Linux kernel with a built-in initramfs whose only program is `/init`. There is no separate bootloader, disk-backed root filesystem, systemd or container engine. The image is reproducible, and its expected measurement is computed offline.

The current implementation runs on **Intel TDX / Google Cloud C3**. See [Trust](#trust) for its security assumptions.

## How it works

```text
Boot VM → Verify empty state → Record signed launch → Verify launch → Commit → Pull & run
```

1. **Boot.** `/init` generates a fresh key for this boot and records it, with your owner public key, in an append-only hardware measurement register.
2. **Verify.** Your machine asks for a quote over a fresh nonce and checks the hardware signature, Google's endorsed firmware, the exact boot image and the ledger. The VM is now verified and empty.
3. **Launch.** The CLI sends your signed container configuration, with secrets encrypted to the boot key. `/init` records it. The CLI verifies that the recorded launch is exactly what it sent, then signs a commit that authorizes the pull.
4. **Run.** `/init` pulls the image by digest, verifies each layer and starts it, with secrets under `/run/secrets`. The CLI verifies the resulting state.

The container cannot extend the measurement register or request quotes over its own data, so it cannot forge this evidence. The [whitepaper](docs/whitepaper.md) explains each check and its assumptions.

## Quick example

After [installing](#install), start and verify a new VM, deploy a public container, read its output, and remove the VM:

```sh
vcontainer run --session demo
vcontainer deploy --session demo hello-world:latest
vcontainer logs --session demo
vcontainer rm --session demo
```

`run` verifies the empty VM. `deploy` resolves the image to a digest, verifies the recorded launch and starts it; this example prints the hello-world message and finishes in state `Exited (exit code 0)`. The VM keeps running after its container exits, and each VM boot accepts one launch, so remove the session when you are done.

For an agent with API keys, use the same flow with `--secret`; see [vcontainer deploy](#vcontainer-deploy) and the [OpenRouter example](examples/openrouter-agent/README.md).

## Install

You need:

- Linux x86-64 with Git, Docker, `rustup` and a C linker. The repository pins Rust 1.95.0.
- A Google Cloud project with the Compute Engine API enabled and C3 quota in your zone.
- `gcloud`, logged in, with permission to manage VMs, images, networks, firewall rules and Cloud Storage.

Set your project (replace `PROJECT` with its ID), clone the repository, build the boot image, install the CLI and publish the image:

```sh
gcloud config set project PROJECT
git clone https://github.com/Mengyuan-L/vContainer && cd vContainer
scripts/build-image.sh
cargo install --locked --path crates/cli
vcontainer publish
```

You do this once per release. The first build downloads the kernel source and the builder's packages. Outputs go to `out/`, including `BOOTX64.EFI` and `disk.raw`; `vcontainer publish` uploads the image and saves its expected measurements.

The release build requires a clean checkout; for local changes, use `scripts/build-image.sh --dev`. `vcontainer measure` prints the built image's measurements without publishing.

Your owner key, release metadata and session files live in `~/.config/vcontainer`. The key is created on first use.

## Usage

A session is one VM, and each VM boot accepts one container launch. The commands below use a session named `agent`.

### vcontainer run

Create a VM, or reconnect to an existing one, and verify it:

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
```

Each ✓ line is one check; all of them must pass. On a `c3-standard-4` this takes about 35 seconds, including VM creation.

The defaults are `us-central1-a` and `c3-standard-4`; change them with `--zone ZONE` and `--machine-type TYPE`. Port 7700 (the attestation API) and the ports you later open with `-p` are reachable only from `--allow-from CIDR`, which defaults to your public IP for a new session. Reconnecting keeps the saved location and access range unless you pass a new `--allow-from`.

If the VM has rebooted, `vcontainer run` reconnects to it empty. Deploy again afterwards.

### vcontainer deploy

The [OpenRouter example](examples/openrouter-agent/README.md) contains a small Python agent and instructions to build and push it. Replace `[REGISTRY]` with the registry path you used, save your API key in `openrouter.key`, then deploy:

```console
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

The CLI verifies the VM, sends the signed launch, verifies that it was recorded, and commits it. The VM then pulls the image and starts the agent, which reads its key from `/run/secrets/OPENROUTER_API_KEY`. Deploying this 43 MB image takes 20–30 seconds. The demo serves HTTP without application authentication; access is limited by the session's firewall.

Options:

- `-p PORT` opens that TCP port to the session's `--allow-from` range, without port remapping.
- `-e KEY=VALUE` sets an environment variable. Its value is public in the evidence.
- `--secret NAME=@FILE`, `--secret NAME=VALUE` or `--secret NAME` (from your environment) encrypts a secret to the VM. It appears at `/run/secrets/NAME`.
- `--user`, `--workdir` and `--entrypoint` override the image's settings; append `-- CMD ARGS...` to override its command.
- `--quiet` keeps container output off the host-readable serial console.

Images must support `linux/amd64` and fit in RAM. Tags are resolved to digests before launch; the digest is what gets measured. Private images work on Artifact Registry and `gcr.io`, using a `gcloud` access token encrypted to the VM. `vcontainer pull` is an alias for `deploy`.

If a deploy was interrupted after the launch reached the VM but before the commit, resume that launch without supplying a new image or options:

```sh
vcontainer deploy --session agent --resume
```

A boot accepts only one launch, even after the container exits. To run another container, remove the session and create it again.

### vcontainer verify

Verify a session at any time, and optionally export its policy for others:

```sh
vcontainer verify --session agent --export-policy agent-policy.json
```

The policy holds the expected measurements, your public key, the instance and the exact launch; it contains no secret values. A third party with this file and network access to port 7700 can verify the session without your private key or cloud account (replace `[IP]` with the session's IP, and choose an `--allow-from` range that admits them):

```sh
vcontainer verify --url http://[IP]:7700 --policy agent-policy.json
```

After a reboot, verification against the old launch policy reports that the launch is missing; reconnect, redeploy and export a new policy.

### vcontainer logs, ls, rm

```sh
vcontainer logs --session agent    # serial output from the kernel, /init and the container
vcontainer ls                      # your sessions and their deployed images
vcontainer rm --session agent      # delete the VM, its firewall rule and the local session record
```

A session bills as a running VM until you remove it. Published images, Cloud Storage objects and the shared network remain after `rm`.

## Trust

On the current platform, vContainer trusts:

- Intel's CPU, TDX module and quoting enclave;
- Google's TDX firmware, checked against its signed launch endorsement;
- the host's default ACPI and KVM paravirtual configuration, which is trusted rather than checked;
- you, the owner.

The host otherwise controls the network, metadata, disk, serial console and registry path. Container isolation protects the evidence; it is not a sandbox for untrusted code.

Only secret values and the registry token are encrypted to the VM, and the container can still leak them. The rest is public:

- **Launch configuration:** image, command, environment variables and secret names are readable in the ledger.
- **Container output:** sent to the host-readable serial console unless you pass `--quiet`.
- **Application traffic:** vContainer does not encrypt it. Have the container serve TLS with a key passed through `--secret`, and publish its fingerprint with `-e` so clients can check it against the verified evidence.

## Status

Version 0.1 works end to end on real Google Cloud C3 hardware. The release from commit `32c2cb4` was rebuilt from scratch, including in a fresh clone, with byte-for-byte matching outputs. Release builds use pinned inputs, freshly extracted sha256-checked kernel source and fixed timestamps; `vcontainer publish` records the source commit and `vcontainer verify` prints it. See [measured results](docs/whitepaper.md#9-measured-on-google-cloud-us-central1-a-october-2026) and [security review findings](docs/whitepaper.md#8-security-analysis).

Current limits:

- One container per VM boot; the root filesystem is in RAM and is lost on reboot, including after host maintenance.
- A minimal kernel: some workloads need features it omits, such as user namespaces or `io_uring`.
- Intel TDX on Google Cloud C3 only. Porting means replacing the platform-specific guest, verifier, cloud, kernel and image-build pieces. There is no SEV-SNP implementation; the [design study](docs/design/DESIGN.md) covers the platform analysis.

## Learn more

- [OpenRouter agent](examples/openrouter-agent/README.md): build and run the example.
- [Whitepaper](docs/whitepaper.md): what verification establishes, its assumptions and the implementation.
- [Boot image](image/README-image.md): kernel configuration, image construction and hardware measurements.
- [Design study](docs/design/DESIGN.md): original design decisions and platform analysis.
- Code: [core](crates/core) (protocol and OCI client), [guest](crates/init) (`/init`) and [CLI](crates/cli) (commands and verifier).
