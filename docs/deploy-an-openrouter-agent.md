# What you need

To deploy the included OpenRouter agent in a confidential VM, you need:

- **An OpenRouter API key:** able to call the model you select. The demo uses `openai/gpt-4o-mini` by default; requests use your OpenRouter account and may incur model charges.
- **A Google Cloud project:** billing enabled, permission to create Artifact Registry repositories, images, buckets, VMs, networks, and firewall rules, and C3 quota in your zone (default: `us-central1-a`).
- **A Linux x86-64 build machine:** Python 3.9+, a native C toolchain, Git, Bash, and Docker running and accessible without `sudo`. The installer sets up Rust automatically; it needs `curl` and `sh` if rustup is missing.
- **The Google Cloud CLI:** `gcloud` installed and access to your project.

You do not need to write an agent. This guide uses [examples/openrouter-agent](../examples/openrouter-agent/): a Python HTTP service that accepts `POST /chat`, calls OpenRouter, and reads its API key from `/run/secrets/OPENROUTER_API_KEY`.

The CLI also works on macOS. Build and publish the boot image on Linux first, then follow the [CLI-only setup](../README.md#cli-only-on-macos-or-another-linux-machine) on your Mac.

## 1. Install vContainer once

Clone the repository if needed, then run this guide's commands from its root:

```sh
git clone https://github.com/Mengyuan-L/vContainer
cd vContainer
```

Replace `YOUR_PROJECT_ID`, sign in, and enable the APIs this guide uses:

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

Add `--dev` to the installer command if the checkout has uncommitted changes. Add `--dry-run` to preview commands. Setup saves `~/.config/vcontainer/release.json`; it does not start a VM. If you have already completed [setup](../README.md#install), keep your project configured and continue below.

## 2. Build and push the demo agent

Set the registry and image name, using your project ID from step 1:

```sh
REGISTRY="us-central1-docker.pkg.dev/$PROJECT/vcontainer"
IMAGE="$REGISTRY/openrouter-agent:v1"
```

Create the repository once (skip creation if it already exists), and let Docker authenticate to it:

```sh
gcloud artifacts repositories create vcontainer --repository-format=docker --location=us-central1
gcloud auth configure-docker us-central1-docker.pkg.dev
```

Build the included demo for `linux/amd64` and push it:

```sh
docker build --platform linux/amd64 -t "$IMAGE" examples/openrouter-agent
docker push "$IMAGE"
```

This is the **agent image**. The **boot image** from step 1 runs vContainer and does not need rebuilding when you change the agent.

## 3. Save your OpenRouter key

Create `openrouter.key` in the repository root, containing only your OpenRouter API key. Restrict access to the file:

```sh
chmod 600 openrouter.key
```

Keep this file outside `examples/openrouter-agent`, so it is outside the Docker build context. The repository ignores `*.key` files. The key is delivered at deployment time; it is never built into the image or put into a public environment variable.

## 4. Start a verified VM and deploy

Start a session named `openrouter`:

```sh
vcontainer run --session openrouter
```

After verification succeeds, deploy the image with your key:

```sh
vcontainer deploy --session openrouter "$IMAGE" -p 8080 --secret OPENROUTER_API_KEY=@openrouter.key -e MODEL=openai/gpt-4o-mini --quiet
```

The CLI encrypts the key to this verified VM, records and verifies the launch, then authorizes the VM to pull and start the agent. The agent reads the key from `/run/secrets/OPENROUTER_API_KEY`. Your private-registry access token is also encrypted to the verified VM.

Change the value after `MODEL=` to use another model available to your OpenRouter account. The model name is part of the public, measured launch; the key value is not. `--quiet` keeps the agent's output off the host-readable serial console.

## 5. Call the agent

Set `IP` to the address printed by `vcontainer deploy`; replace this example:

```sh
IP=203.0.113.7
```

Check that the service is reachable without making a model request:

```sh
curl "http://$IP:8080/"
```

Then send a chat message:

```sh
curl -X POST "http://$IP:8080/chat" -H 'Content-Type: application/json' -d '{"message":"Reply with exactly: vContainer works"}'
```

The response is JSON with `model` and `reply` fields. If the agent returns HTTP 502, check that your API key can access the selected model; the JSON error reports the upstream failure.

This demo uses plain HTTP and has no application authentication. Its port is open only to your public IP by default. Anyone allowed to reach it can spend your OpenRouter key, and the cloud host can read client traffic. Add authentication and TLS to the agent before using it for private conversations. The outbound request goes to OpenRouter over HTTPS; OpenRouter receives the prompt and your API key. vContainer does not implement egress filtering. See [Trust](../README.md#trust).

## 6. Verify the deployment

```sh
vcontainer verify --session openrouter --export-policy openrouter-policy.json
```

The policy contains the expected boot measurements and exact agent launch, including the image digest and model, but no secret values. Share it with someone who wants to verify the deployment; they also need network access to the VM's attestation port (7700). See the [verification commands](command-reference.md).

## 7. Remove the VM

```sh
vcontainer rm --session openrouter
```

The VM keeps billing even after the agent exits. This removes the VM and its session firewall rule; registry images, the boot image, and storage objects remain in your project.

To change the model or agent image, remove the session and repeat step 4 with your new settings. To adapt the demo to your own application, follow [Deploy your own agent](deploy-your-agent.md).
