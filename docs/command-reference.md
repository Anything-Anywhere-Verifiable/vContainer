# Command reference

Add **`--details`** to `run`, `deploy` or `verify` for all verification claims, complete release metadata, and detailed verification errors. The default terminal view groups the checks and displays the complete matching boot measurements, pinned image digest, selected manifest, and final state.

**`vcontainer run --session NAME`** creates a VM, or reconnects to an existing one, and verifies it. Options:

- `--zone ZONE` and `--machine-type TYPE` choose where and what to run (defaults: `us-central1-a`, `c3-standard-4`).
- `--allow-from CIDR` sets who can reach the attestation port (7700) and the container's ports (default: your public IP).

`--allow-from` and published ports control incoming connections only. vContainer does not implement outbound destination filtering or an egress allowlist.

**`vcontainer deploy --session NAME IMAGE`** verifies the session, sends the launch, verifies that the VM recorded it, and then lets the VM pull and start the image. `vcontainer pull` is an alias. Options:

- `-p PORT` opens that TCP port to the `--allow-from` range; there is no port remapping.
- `-e KEY=VALUE` sets an environment variable. Variables are public (see [Trust](security.md)).
- `--secret NAME=@FILE`, `--secret NAME=VALUE` or `--secret NAME` (taken from your environment) encrypts a secret to the VM. It appears as `/run/secrets/NAME`.
- `--user`, `--workdir` and `--entrypoint` override the image's settings, and `-- CMD ARGS...` at the end overrides its command.
- `--quiet` keeps container output off the serial console.
- `--resume` finishes a deploy interrupted after its launch reached the VM but before commit. Use it without an image or new launch options.

`IMAGE` is a full reference, `HOST/REPOSITORY:TAG` or `HOST/REPOSITORY@DIGEST`; nothing is filled in for you. Images must be linux/amd64 and fit in RAM. The kernel is minimal; for example, it has no user namespaces or io_uring. A tag is resolved to a digest before launch, and the digest is what gets measured. Public images are pulled without credentials (Docker Hub is tested); private images work on Artifact Registry and gcr.io, where your `gcloud` access token is sealed to the VM.

**`vcontainer verify --session NAME`** rechecks a session at any time. Add `--export-policy FILE` to export its public expected values; use `--url URL --policy FILE` for independent verification. See [Verify and share](verify-and-share.md) for the complete workflow.

**`vcontainer logs --session NAME`** prints the serial console: kernel, `/init` and container output. **`vcontainer ls`** lists your sessions and what each one runs. **`vcontainer rm --session NAME`** deletes the VM, its firewall rule, and its local session record. Published images, Cloud Storage artifacts, and the shared network remain.

