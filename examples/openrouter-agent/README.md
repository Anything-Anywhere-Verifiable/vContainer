# OpenRouter demo agent

A 40-line Python agent: `POST /chat {"message": "..."}` answers through OpenRouter.
The API key is sealed to the verified VM and read from `/run/secrets/OPENROUTER_API_KEY`;
it is never in the image, the launch config or instance metadata.

```sh
docker build --platform linux/amd64 -t REGION-docker.pkg.dev/PROJECT/REPO/openrouter-agent:v1 .
docker push REGION-docker.pkg.dev/PROJECT/REPO/openrouter-agent:v1
vcontainer run --session agent
vcontainer deploy --session agent REGION-docker.pkg.dev/PROJECT/REPO/openrouter-agent:v1 \
    -p 8080 --secret OPENROUTER_API_KEY=@openrouter.key   # optional: -e MODEL=anthropic/claude-sonnet-4.5
curl -X POST http://IP:8080/chat -d '{"message":"hello"}'
```

`/chat` has no authentication: anyone who can reach port 8080 can spend your key.
`vcontainer` opens the port only to your own IP (`--allow-from` on `run` changes that).
