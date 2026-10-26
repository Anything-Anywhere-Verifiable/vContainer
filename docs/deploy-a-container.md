# What you need

To deploy a container in a confidential VM with vContainer, you need:

- **A container image:** a full registry reference for a `linux/amd64` image that fits in the VM's RAM. This guide uses the public `docker.io/library/hello-world:latest` image, so you do not need application code or registry credentials.
- **A Google Cloud project:** billing enabled, Compute Engine permissions, permission to create storage buckets and objects, and C3 quota in your zone (default: `us-central1-a`).
- **A Linux x86-64 build machine:** Python 3.9+, a native C toolchain, Git, Bash, and Docker running and accessible without `sudo`. The installer sets up Rust automatically; it needs `curl` and `sh` if rustup is missing.
- **The Google Cloud CLI:** `gcloud` installed and access to your project.

The CLI also works on macOS. Build and publish the boot image on Linux first, then follow the [CLI-only setup](../README.md#cli-only-on-macos-or-another-linux-machine) on your Mac.

## 1. Install vContainer once

Clone the repository if needed, then run the remaining commands from its root:

```sh
git clone https://github.com/Anything-Anywhere-Verifiable/vContainer
cd vContainer
```

Replace `YOUR_PROJECT_ID`, sign in, and enable Compute Engine:

```sh
PROJECT=YOUR_PROJECT_ID
gcloud auth login
gcloud config set project "$PROJECT"
gcloud services enable compute.googleapis.com
```

Build and publish the boot image, and install the CLI:

```sh
python3 scripts/install.py --publish --project "$PROJECT"
export PATH="$HOME/.cargo/bin:$PATH"
vcontainer --help
```

Add `--dev` to the installer command if the checkout has uncommitted changes. Add `--dry-run` to preview commands. This publishes a boot image and saves `~/.config/vcontainer/release.json`; it does not start a VM. If you have already completed [setup](../README.md#install), skip to step 2.

## 2. Choose your container image

Use hello-world for a first deployment:

```sh
IMAGE=docker.io/library/hello-world:latest
```

You can replace this with your own image. Always include the registry host, repository, and tag or digest, such as `HOST/REPOSITORY:TAG` or `HOST/REPOSITORY@sha256:DIGEST`. The CLI resolves a tag to a digest before launch.

Public Docker Hub images work without credentials. For private images, Artifact Registry and gcr.io are supported using your `gcloud` access token, encrypted to the verified VM. See [building and pushing your own image](deploy-your-agent.md#3-build-and-push-your-agent-image) for an Artifact Registry example.

## 3. Create a verified VM

Start a session named `container`:

```sh
vcontainer run --session container
```

The CLI creates a confidential VM, verifies its firmware and boot image, and checks that it is empty. Wait for the session to pass verification.

## 4. Deploy the container

```sh
vcontainer deploy --session container "$IMAGE"
```

vContainer sends the signed launch, verifies that the VM recorded exactly that launch, then authorizes the VM to pull the image by digest and start it.

Hello-world prints a message and exits. An `Exited` state with exit code 0 is expected. The VM remains allocated until you remove the session.

For your own container, add the options it needs to this deploy command:

| Option | Purpose |
|---|---|
| `-p 8080` | Allow inbound TCP connections to the container's port 8080 from your public IP. A service must listen on `0.0.0.0`. |
| `-e NAME=VALUE` | Set a public, measured environment variable. |
| `--secret NAME=@local.key` | Encrypt a local file to the VM; the container reads it at `/run/secrets/NAME`. |
| `--quiet` | Keep container output off the host-readable serial console. |
| `--entrypoint PROGRAM` and `-- CMD ARGS...` | Override the image's entrypoint or command. |

Ports are opened at the same number in the VM; `-p` does not take Docker's `HOST:CONTAINER` mapping syntax. Incoming access defaults to your public IP; `--allow-from CIDR` on `run` changes that range. There is no built-in egress filtering or application TLS. See [Trust](../README.md#trust).

## 5. Communicate with the container

Connect to the service your container exposes, using the VM's external IP and the port you opened with `-p`. For an HTTP service listening on `0.0.0.0:8080`, deploy with `-p 8080` and use `http://VM_IP:8080` in your browser or HTTP client. Use your application's own paths and request format. Declaring `EXPOSE` in a Dockerfile alone does not open the firewall port.

Hello-world exits without starting a network service, so you communicate with it only by reading its output in step 6. To try a running HTTP service after the hello-world example, remove its completed session, create a fresh one, and start Python's demo HTTP server:

```sh
vcontainer rm --session container
vcontainer run --session container
vcontainer deploy --session container docker.io/library/python:3.13-slim-trixie -p 8080 --entrypoint python3 -- -m http.server 8080 --bind 0.0.0.0 --directory /tmp
```

The deploy command prints the VM's address, for example:

```console
  port 8080: http://203.0.113.7:8080
```

Replace the example IP with that address and send a request from the same machine you used to create the session:

```sh
IP=203.0.113.7
curl "http://$IP:8080/"
```

This server returns a directory listing of its `/tmp` directory. For your own container, replace the demo server image and command with your application. Opening a port does not add an HTTP server; for other TCP protocols, use the appropriate client and the same VM IP and port.

Incoming connections are allowed from your public IP by default. To use a client on another network, set `--allow-from CIDR` when creating the session, using that client's public IP or network range. If a connection fails, check that the container is still running, the service listens on `0.0.0.0` rather than `127.0.0.1`, the matching port was passed to `deploy`, and the client is in the allowed range.

Port 7700 is vContainer's attestation API; use your published application port for application requests. This demo uses plain HTTP. For private traffic, configure TLS and authentication in your application and connect to its HTTPS endpoint. vContainer does not provide an interactive shell or forward your terminal's input to the container.

## 6. Read output and verify the deployment

Read hello-world's message from the serial console:

```sh
vcontainer logs --session container
```

Container output is visible to the cloud host unless you deploy with `--quiet`. Do not print secrets to it.

Recheck the session and export a policy containing the expected measurements and exact launch, without secret values:

```sh
vcontainer verify --session container --export-policy container-policy.json
```

You can also list local sessions:

```sh
vcontainer ls
```

## 7. Remove the VM

```sh
vcontainer rm --session container
```

This deletes the VM and its session firewall rule. The VM keeps billing even after the container exits. Published boot images and storage objects remain in your project.

Each VM boot runs one container. To deploy another image, remove the session, choose a new `IMAGE`, then repeat steps 3 and 4. You do not need to rebuild the boot image.

To deploy an agent next, try the [pull request review agent](../examples/pr-review-agent/README.md) or follow [Deploy your own agent](deploy-your-agent.md).
