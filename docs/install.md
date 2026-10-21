# Installation

Build and publish the vContainer boot image once, then deploy application containers with the CLI. The boot image contains Linux and `/init`; changing your application does not require rebuilding it.

Application images must target `linux/amd64` and fit in the VM's RAM.

The boot image is built on **Linux x86-64**. The CLI also runs on macOS; see [CLI-only setup](#cli-only-on-macos-or-another-linux-machine).

## Prerequisites

- Python 3.9+, a native C compiler/linker, Git, Bash, and a running Docker daemon accessible without `sudo`.
- `curl` and `sh` if rustup is not installed. The installer prepares Rust automatically, using the version pinned in `rust-toolchain.toml` (1.95.0).
- `gcloud` and a Google Cloud project with billing enabled, Compute Engine enabled, and C3 quota in your zone. You need permissions to manage VMs, images, networks, firewall rules, and Cloud Storage buckets and objects.

The installer sets up Rust and checks the other prerequisites. It leaves your global Rust default and shell startup files unchanged. Install the other tools yourself before running it.

## Build, install, and publish on Linux x86-64

Clone the repository and enter it:

```sh
git clone https://github.com/Mengyuan-L/vContainer && cd vContainer
```

Sign in, set your default project, and enable Compute Engine. Replace `PROJECT` with your project ID:

```sh
gcloud auth login &&
  gcloud config set project PROJECT &&
  gcloud services enable compute.googleapis.com
```

Build the boot image, install the CLI, and publish the image to that project:

```sh
python3 scripts/install.py --publish --project PROJECT
```

The first build downloads Rust dependencies, the kernel source, and builder packages. Publishing creates a Cloud Storage bucket if needed, uploads the boot disk, and creates a Compute Engine image. **It does not start a VM**; `vcontainer run` does that.

The installer uses `--project` only for that setup invocation, which is why the default project is set separately above. It installs `vcontainer` under `~/.cargo/bin` by default; make it available in this terminal if needed:

```sh
export PATH="$HOME/.cargo/bin:$PATH"
```

You are ready for the [hello-world example](../README.md#quick-start).

<details>
<summary>Setup options</summary>

| Setup option | What it does |
| --- | --- |
| No flags | Builds the boot image and installs the CLI; does not access Google Cloud. |
| `--publish --project PROJECT` | Also publishes the boot image and saves the local release settings. |
| `--cli-only` | Installs only the CLI, on Linux or macOS. |
| `--dev` | Allows uncommitted changes; a modified checkout is not recorded as a release from a clean commit. |
| `--clean` | Drops cached kernel source and objects. Clean release builds already do this. |
| `--install-root PATH` | Installs the executable under `PATH/bin`; otherwise uses `CARGO_INSTALL_ROOT`, `CARGO_HOME`, or `~/.cargo`. |
| `--dry-run` | Prints setup commands without executing them or checking prerequisites. |

For example, preview the complete setup with:

```sh
python3 scripts/install.py --publish --project PROJECT --dry-run
```

Release builds require a clean checkout. While changing code or documentation, add `--dev` to the installer command. See [scripts/install.py](../scripts/install.py) for the setup workflow and [image/README-image.md](../image/README-image.md) for the separate hardware probe workflow.

</details>

## CLI only on macOS or another Linux machine

<details>
<summary>Install the CLI on your client machine</summary>

Build and publish the boot image on Linux x86-64 first. On the client machine, install Python, a native C toolchain, and `gcloud`, then run this in a checkout:

```sh
python3 scripts/install.py --cli-only
```

Copy `~/.config/vcontainer/release.json` from the build machine to that same path on the client, authenticate `gcloud`, and set the same default project. Add the CLI's installation directory to `PATH` if needed. Each client creates its own owner key on first use; you do not need to copy the builder's private key.

</details>

Boot artifacts are saved in `out/`, including `BOOTX64.EFI`, `disk.raw`, measurements, and build metadata. Published release settings, your owner key, and session files live in `~/.config/vcontainer`.

<details>
<summary>Equivalent manual build, install, and publish commands</summary>

With Rust and the other prerequisites installed, run this from the repository root:

```sh
scripts/build-image.sh &&
  cargo install --locked --path crates/cli &&
  gcloud config set project PROJECT &&
  vcontainer publish
```

To inspect an existing build's measurements without publishing:

```sh
vcontainer measure
```

</details>

