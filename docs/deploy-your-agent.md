# What you need

To deploy your own agent with vContainer, you need:

- **Your agent as a container:** a Dockerfile that builds a `linux/amd64` image. The agent and its filesystem must fit in the VM's RAM. You can start from the [OpenRouter demo](../examples/openrouter-agent/) if you do not have an image yet.
- **A Google Cloud project:** billing enabled, permission to create images, buckets, VMs, networks, and firewall rules, and C3 quota in your chosen zone (the default is `us-central1-a`). This guide uses Artifact Registry to store your agent image.
- **A Linux x86-64 build machine:** Python 3.9+, a native C toolchain, Git, Bash, and a running Docker daemon. The installer sets up Rust automatically; it needs `curl` and `sh` if rustup is missing.
- **The Google Cloud CLI:** `gcloud` installed and access to your project.
- **Your agent's API keys:** saved locally, outside its source directory and Docker build context. vContainer delivers them as files in `/run/secrets`.

The CLI also runs on macOS. Build and publish vContainer's boot image on Linux first, then follow the [CLI-only setup](../README.md#cli-only-on-macos-or-another-linux-machine) on your Mac.

To start with a ready-made workload, use [Deploy a container](deploy-a-container.md) or [Deploy an OpenRouter agent](deploy-an-openrouter-agent.md).

## 1. Install vContainer once

Clone the repository if you have not already, and run this guide's commands from its root:

```sh
git clone https://github.com/Mengyuan-L/vContainer
cd vContainer
```

Choose your project, sign in, and enable the APIs this guide uses. Replace `YOUR_PROJECT_ID`:

```sh
PROJECT=YOUR_PROJECT_ID
gcloud auth login
gcloud config set project "$PROJECT"
gcloud services enable compute.googleapis.com artifactregistry.googleapis.com
```

Build and publish the boot image, and install the CLI:

```sh
python3 scripts/install.py --publish --project "$PROJECT"
export PATH="$HOME/.cargo/bin:$PATH"
vcontainer --help
```

If you have uncommitted changes in the repository, add `--dev` to the installer command. Use `--dry-run` to preview setup commands. See [installation options](install.md) for more detail.

Setup creates the cloud boot image and saves its expected measurements in `~/.config/vcontainer/release.json`. It does not start an agent VM. If you have already completed setup, skip to step 2.

## 2. Prepare your agent

Your container's default command should start the agent. For an HTTP agent, have it listen on `0.0.0.0` (this guide uses port 8080).

Read API keys from secret files rather than including them in your image or passing them through `-e`. For example, a Python agent can read a key with:

```python
from pathlib import Path

api_key = Path("/run/secrets/MODEL_API_KEY").read_text().strip()
```

Use the secret name your application expects. vContainer does not convert secret files to environment variables. The [command reference](command-reference.md) explains how to override an image's entrypoint or command if needed.

Choose the model, prompts, tools, authentication, and any TLS setup in your agent code or configuration. vContainer handles deployment and verification of that container.

## 3. Build and push your agent image

Set the registry and image name. Keep using the `PROJECT` value from step 1:

```sh
REGISTRY="us-central1-docker.pkg.dev/$PROJECT/vcontainer"
IMAGE="$REGISTRY/my-agent:v1"
```

Create the repository once (skip creation if it already exists), and let Docker authenticate:

```sh
gcloud artifacts repositories create vcontainer --repository-format=docker --location=us-central1
gcloud auth configure-docker us-central1-docker.pkg.dev
```

Point `AGENT_DIR` at the directory containing your agent's Dockerfile, then build and push. Replace the example path:

```sh
AGENT_DIR=/absolute/path/to/your/agent
docker build --platform linux/amd64 -t "$IMAGE" "$AGENT_DIR"
docker push "$IMAGE"
```

This is your **agent image**. It is separate from the **boot image** built in step 1. Updating your agent does not require rebuilding vContainer.

## 4. Save your API key locally

Create `model.key` in your current directory with only the API key, then restrict access to it:

```sh
chmod 600 model.key
```

Keep this file outside `AGENT_DIR` and out of version control. The repository ignores `*.key` files. Do not put the key in a Dockerfile or image layer.

## 5. Start a verified VM and deploy

Create a session named `my-agent`:

```sh
vcontainer run --session my-agent
```

The CLI creates a confidential VM and verifies its firmware, boot image, and empty state. Wait for verification to succeed before deploying:

```sh
vcontainer deploy --session my-agent "$IMAGE" -p 8080 --secret MODEL_API_KEY=@model.key --quiet
```

This resolves the image tag to a digest, sends a signed launch with the encrypted key, verifies the recorded launch, and authorizes the VM to pull and start the image. For this private registry, your `gcloud` registry access token is also encrypted to the verified VM.

`MODEL_API_KEY` appears at `/run/secrets/MODEL_API_KEY`. `-p 8080` allows inbound connections to that port from your public IP. `--quiet` keeps your agent's output off the cloud host's readable serial console. Omit `-p` for an agent that does not serve incoming requests.

This protects the agent's memory from the cloud host. Requests and responses need application TLS to stay private, and vContainer does not filter outbound connections. The agent can still send data to its model provider or any other reachable service; see [Trust](../README.md#trust).

## 6. Use and verify your agent

The deploy command prints the VM's IP and port. Connect using your agent's client or API. For an agent that implements the demo's `POST /chat` endpoint, replace the example IP and run:

```sh
IP=203.0.113.7
curl -X POST "http://$IP:8080/chat" -H 'Content-Type: application/json' -d '{"message":"hello"}'
```

That example uses plain HTTP. Use your agent's HTTPS endpoint and authentication for private conversations.

Recheck the session and export a policy file:

```sh
vcontainer verify --session my-agent --export-policy my-agent-policy.json
```

You can share this policy with someone who wants to verify the deployment. It contains the expected measurements, image and launch configuration, and owner public key, but no secret values. They also need access to the VM's attestation port (7700); see [verification commands](command-reference.md).

## 7. Update or remove the deployment

A VM boot runs one container. For a fresh deployment after changing your agent, remove the session, build and push your new image tag, then repeat step 5 with the new `IMAGE` value.

When you are finished, remove the session:

```sh
vcontainer rm --session my-agent
```

The VM keeps billing even after its container exits. This removes the VM and its session firewall rule; published boot images, registry images, and storage objects remain in your project.
