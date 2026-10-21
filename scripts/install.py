#!/usr/bin/env python3
"""Build the boot image and install the CLI; optionally publish to Google Cloud.

Uses Python 3.9+ and the standard library. Run --help for prerequisites/options.
"""

import argparse
import os
from pathlib import Path
import platform
import shlex
import shutil
import subprocess
import sys
import tempfile


REPO = Path(__file__).resolve().parent.parent


def arguments(argv=None):
    parser = argparse.ArgumentParser(
        description=__doc__,
        epilog=(
            "Rustup and the repository's Rust toolchain are installed automatically. "
            "Prerequisites: a native linker; curl and sh if rustup is missing; for the boot image, "
            "Linux x86-64, Git, Bash and a running Docker daemon. Publishing also "
            "requires authenticated gcloud and a project with Compute Engine enabled. "
            "Other dependencies are checked, not installed. Diagnostic probe scripts are "
            "separate; see image/README-image.md."
        ),
    )
    parser.add_argument("--cli-only", action="store_true", help="install only the CLI (Linux or macOS)")
    parser.add_argument("--dev", action="store_true", help="allow an uncommitted checkout for the boot image")
    parser.add_argument("--clean", action="store_true", help="rebuild cached kernel source/objects from scratch")
    parser.add_argument("--publish", action="store_true", help="upload the boot image and create a GCE image/bucket")
    parser.add_argument("--project", help="Google Cloud project ID (required with --publish)")
    parser.add_argument("--dry-run", action="store_true", help="print commands without executing or checking prerequisites")
    # Pass an explicit root so the following steps use the exact binary just installed,
    # regardless of PATH or Cargo's per-machine install configuration.
    cargo_home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    parser.add_argument(
        "--install-root", type=Path,
        default=Path(os.environ.get("CARGO_INSTALL_ROOT", cargo_home)),
        help="Cargo install root (default: CARGO_INSTALL_ROOT, CARGO_HOME, or ~/.cargo)",
    )
    args = parser.parse_args(argv)
    if args.cli_only and (args.dev or args.clean or args.publish):
        parser.error("--cli-only cannot be combined with --dev, --clean, or --publish")
    if args.publish and not args.project:
        parser.error("--publish requires --project PROJECT")
    if args.project and not args.publish:
        parser.error("--project requires --publish")
    args.install_root = args.install_root.expanduser().resolve()
    return args


def run(command, env, dry_run=False):
    command = [str(part) for part in command]
    print("+ " + shlex.join(command), flush=True)
    if not dry_run:
        subprocess.run(command, cwd=REPO, env=env, check=True)


def preflight(args, env):
    if platform.system() not in ("Linux", "Darwin"):
        raise RuntimeError("The CLI installer supports Linux and macOS.")
    if not args.cli_only and (platform.system() != "Linux" or platform.machine() not in ("x86_64", "AMD64")):
        raise RuntimeError("Boot image builds require Linux x86-64. Use --cli-only on this machine.")
    required = {}
    if not args.cli_only:
        required.update({"git": "Install Git.", "bash": "Install Bash.", "docker": "Install and start Docker."})
    if args.publish:
        required["gcloud"] = "Install the Google Cloud CLI and run gcloud auth login."
    for name, hint in required.items():
        if shutil.which(name) is None:
            raise RuntimeError(f"Missing {name}. {hint}")
    if not args.cli_only:
        changes = subprocess.run(
            ["git", "status", "--porcelain"], cwd=REPO, env=env,
            check=True, capture_output=True, text=True,
        ).stdout
        if changes.strip() and not args.dev:
            raise RuntimeError("Release builds require a clean checkout. Commit changes or use --dev.")
        run(["docker", "info", "--format", "{{.ServerVersion}}"], env)
    if args.publish:
        # Check credentials without printing the access token to the terminal/logs.
        subprocess.run(
            ["gcloud", "auth", "print-access-token"], cwd=REPO, env=env,
            check=True, stdout=subprocess.DEVNULL,
        )


def ensure_rust(env, dry_run=False):
    # rustup-init installs proxies here. Update this process only, so Cargo works
    # immediately even when the caller's shell has never sourced ~/.cargo/env.
    cargo_bin = Path(env.get("CARGO_HOME", Path.home() / ".cargo")).expanduser().resolve() / "bin"
    env["PATH"] = str(cargo_bin) + os.pathsep + env.get("PATH", os.defpath)
    rustup = shutil.which("rustup", path=env["PATH"])
    if rustup is None:
        if not dry_run:
            for name in ("curl", "sh"):
                if shutil.which(name, path=env["PATH"]) is None:
                    raise RuntimeError(f"Missing {name}; install it to bootstrap rustup automatically.")
        print("Installing rustup from https://sh.rustup.rs", flush=True)

        def bootstrap(script):
            run([
                "curl", "--proto", "=https", "--tlsv1.2", "-sSf",
                "https://sh.rustup.rs", "-o", script,
            ], env, dry_run)
            run([
                "sh", script, "-y", "--no-modify-path", "--profile", "minimal",
                "--default-toolchain", "none",
            ], env, dry_run)

        if dry_run:
            bootstrap("<temporary rustup-init.sh>")
        else:
            with tempfile.TemporaryDirectory(prefix="vcontainer-rustup-") as temporary:
                bootstrap(Path(temporary) / "rustup-init.sh")
        rustup = str(cargo_bin / "rustup")
    # With no toolchain argument, rustup reads rust-toolchain.toml from cwd,
    # including its targets/components. Keep the user's global default unchanged.
    run([rustup, "toolchain", "install", "--profile", "minimal", "--no-self-update"], env, dry_run)
    run(["cargo", "--version"], env, dry_run)


def main(argv=None):
    args = arguments(argv)
    env = os.environ.copy()
    if args.publish:
        env["CLOUDSDK_CORE_PROJECT"] = args.project
        print(f"Publishing to Google Cloud project: {args.project}", flush=True)
    try:
        if not args.dry_run:
            preflight(args, env)
        ensure_rust(env, args.dry_run)
        if not args.cli_only:
            build = ["bash", REPO / "scripts/build-image.sh"]
            if args.dev:
                build.append("--dev")
            if args.clean:
                build.append("--clean")
            run(build, env, args.dry_run)
        run([
            "cargo", "install", "--locked", "--path", REPO / "crates/cli",
            "--root", args.install_root,
        ], env, args.dry_run)
        binary = args.install_root / "bin/vcontainer"
        run([binary, "--help"], env, args.dry_run)
        if not args.cli_only:
            run([binary, "measure", "--dir", REPO / "out"], env, args.dry_run)
        if args.publish:
            run([binary, "publish", "--dir", REPO / "out"], env, args.dry_run)
    except subprocess.CalledProcessError as error:
        print(f"install: command failed (exit {error.returncode}); stopping.", file=sys.stderr)
        return error.returncode if 0 < error.returncode < 256 else 1
    except (OSError, RuntimeError) as error:
        print(f"install: {error}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print("\ninstall: interrupted.", file=sys.stderr)
        return 130

    if args.dry_run:
        print("Preview only; no commands executed.")
        return 0
    print(f"\nCLI installed: {binary}")
    print(f"Add {binary.parent} to PATH if needed, then run: vcontainer --help")
    if args.publish:
        print("Boot image published; release settings saved in ~/.config/vcontainer/release.json.")
        print("Next: vcontainer run --session demo")
    elif not args.cli_only:
        print("Boot image built in out/. Next: vcontainer publish (with gcloud configured).")
    else:
        print("To run sessions, copy release.json from a machine that built and published the boot image.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
