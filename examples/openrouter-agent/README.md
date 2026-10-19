# OpenRouter demo agent

A 40-line Python agent: `POST /chat {"message": "..."}` answers through OpenRouter.
The API key is sealed to the verified VM and read from `/run/secrets/OPENROUTER_API_KEY`;
it is never in the image, the launch config or instance metadata.

Each block below is one command. Run them from this directory, with your OpenRouter key saved in `openrouter.key`.

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

Start a verified session and deploy the agent into it:

```sh
vcontainer run --session agent
```

```sh
vcontainer deploy --session agent "$REGISTRY/openrouter-agent:v1" -p 8080 --secret OPENROUTER_API_KEY=@openrouter.key
```

To use another model, add `-e MODEL=anthropic/claude-sonnet-4.5` to the deploy command (the default is `openai/gpt-4o-mini`).

Set `IP` to the address that `vcontainer deploy` printed, then chat with the agent:

```sh
curl -X POST "http://$IP:8080/chat" -d '{"message":"hello"}'
```

`/chat` has no authentication: anyone who can reach port 8080 can spend your key.
`vcontainer` opens the port only to your own IP (`--allow-from` on `vcontainer run` changes that).
