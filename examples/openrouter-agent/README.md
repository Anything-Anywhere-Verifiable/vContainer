# OpenRouter demo agent

A 40-line Python agent: `POST /chat {"message": "..."}` answers through OpenRouter.
The API key is sealed to the verified VM and read from `/run/secrets/OPENROUTER_API_KEY`;
it is never in the image, the launch config or instance metadata. The agent reads
the key and uses it to call OpenRouter, so you still need to trust the agent code.

## Before you start

For a walkthrough from prerequisites through cleanup, follow [Deploy an OpenRouter agent](../../docs/deploy-an-openrouter-agent.md). That guide runs commands from the repository root; the commands on this page run from this example directory.

For the general workflow with your own code, follow [Deploy your own agent](../../docs/deploy-your-agent.md). This page uses the included OpenRouter demo.

Complete the [main setup](../../README.md#install) first: build and publish the
boot image, install `vcontainer`, and configure `gcloud` for your project. Confirm
that `vcontainer --help` works and `~/.config/vcontainer/release.json` exists.
Also enable Artifact Registry in your project and have Docker available to build
and push this demo's application image.

The boot image runs vContainer; the image you build below runs this Python agent.
You do not rebuild the boot image when changing the agent.

From the repository root, change to this directory:

```sh
cd examples/openrouter-agent
```

Run the remaining commands here. Save your OpenRouter API key in `openrouter.key`,
containing only the key, and restrict access to the file:

```sh
chmod 600 openrouter.key
```

## Build and push the application image

Set `REGISTRY` to your Artifact Registry repository. `docker push` sends the image to whatever host and repository its name starts with, so this is where it goes. It is a plain shell variable: `vcontainer` never reads it, and it lasts only for this terminal. Replace `PROJECT` with your project ID:

```sh
REGISTRY=us-central1-docker.pkg.dev/PROJECT/vcontainer
```

Create the repository (once per project):

```sh
gcloud artifacts repositories create vcontainer --repository-format=docker --location=us-central1
```

Let Docker authenticate to it through `gcloud` (once per machine; without it the push fails with "Unauthenticated request"):

```sh
gcloud auth configure-docker us-central1-docker.pkg.dev
```

Build and push the image:

```sh
docker build --platform linux/amd64 -t "$REGISTRY/openrouter-agent:v1" .
```

```sh
docker push "$REGISTRY/openrouter-agent:v1"
```

## Run and deploy

Start a verified session and deploy the agent into it. `run` creates the VM;
`deploy` sends the measured application launch and encrypted key:

```sh
vcontainer run --session agent
```

```sh
vcontainer deploy --session agent "$REGISTRY/openrouter-agent:v1" -p 8080 --secret OPENROUTER_API_KEY=@openrouter.key
```

To use another model, add `-e MODEL=anthropic/claude-sonnet-4.5` to the deploy command (the default is `openai/gpt-4o-mini`).

## Call the agent and clean up

Set `IP` to the address that `vcontainer deploy` printed (replace this example):

```sh
IP=203.0.113.7
```

Then chat with the agent:

```sh
curl -X POST "http://$IP:8080/chat" -d '{"message":"hello"}'
```

`/chat` has no authentication: anyone who can reach port 8080 can spend your key.
`vcontainer` opens the port only to your own IP (`--allow-from` on `vcontainer run` changes that).
Traffic to this demo is plain HTTP; it is visible to the cloud host. There is no
egress filtering: the agent needs outbound access to OpenRouter. See the main
README's [trust limits](../../README.md#trust).

The VM keeps billing after the container exits. Remove it when finished:

```sh
vcontainer rm --session agent
```
