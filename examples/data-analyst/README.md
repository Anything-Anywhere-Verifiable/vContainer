# Private data analyst

An internal service that answers questions about a sensitive dataset without showing anyone its rows. A data custodian, such as the security team, deploys it with a dataset of support tickets that holds customer names and emails. Teammates ask it questions over HTTPS, such as "Why did first response times get worse in September?", and get answers built from counts and averages.

The agent is one file of Python with no dependencies, [agent.py](agent.py), and teammates ask with [ask.py](ask.py), which verifies the service first. [tickets.csv](tickets.csv) holds 310 synthetic tickets with fake customers.

## What leaves the VM

The model chooses among three tools, and the tools are the only way it sees the data:

| Tool | What it returns |
|---|---|
| `describe` | the number of tickets, the category columns with their allowed values, and the number columns |
| `count` | the number of tickets per group |
| `average` | the mean of `first_response_hours` or `resolution_hours` per group |

Groups are formed by up to two category columns (`product`, `channel`, `priority`, `team`, `status` and the month of `created`), optionally filtered by their values. A group of fewer than 5 tickets shows `<5` and no statistics, and so does `describe` for a dataset of fewer than 5 tickets.

These rules are enforced in the code, not in the prompt:

- The agent loads only the category and number columns. The names, emails and ticket IDs never reach a tool.
- Each category column has a fixed list of allowed values in `agent.py`, `created` must start with a month in the form `YYYY-MM`, and the number columns take only finite numbers of zero or more. A dataset with any other value stops the agent at startup, so no free text can become a group label.

The service does not return raw rows or include them in model prompts. Aggregates can still reveal facts about individuals across several questions, so this is not a statistical privacy guarantee.

Who sees what:

- **OpenRouter and the model provider** see each question and the aggregates the tools return.
- **Callers** see the answers.
- **The cloud host** sees none of the data, questions or answers. The dataset and keys are sealed to the verified VM, and traffic is TLS that ends inside the container. The console shows only the number of tickets and the model at startup, and per question the number of tool calls, such as `answered a question with 10 tool calls`.

Each question is answered on its own: nothing is kept between questions, and the service handles one at a time.

## Inputs

| Name | Kind | Meaning |
|---|---|---|
| `TICKETS` | secret | the dataset, a CSV with the columns of [tickets.csv](tickets.csv) |
| `OPENROUTER_API_KEY` | secret | the model key |
| `API_TOKEN` | secret | the bearer token callers send, at least 32 characters |
| `TLS_CERT`, `TLS_KEY` | secret | the service's certificate and private key |
| `TLS_CERT_SHA256` | environment | the SHA-256 of the certificate in DER form; the agent refuses to start if it does not match `TLS_CERT` |
| `MODEL` | environment, optional | an OpenRouter model ID (default `anthropic/claude-sonnet-5.5`) |

`TLS_CERT_SHA256` is part of the measured launch, so `vcontainer verify` shows which certificate the verified service holds.

## Deploy it (the custodian)

The commands below deploy the image we publish, pinned by digest, so nothing needs to be built; to run your own build instead, see [Build your own image](#build-your-own-image). Run them from this directory, with your OpenRouter key in `openrouter.key`.

Start a session. `--allow-from` sets who can reach the service and its attestation port; use a range that covers your team and your own machine:

```sh
vcontainer run --session analyst --allow-from 203.0.113.0/24
```

Create a certificate, a token for callers, and the certificate's fingerprint. Callers check the certificate by this fingerprint, so it does not name the session's address. Files named `*.key` are ignored by Git, and [.dockerignore](.dockerignore) keeps everything but `agent.py` out of the Docker build:

```sh
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 365 -subj /CN=data-analyst -keyout analyst.key -out analyst.crt
openssl rand -hex 32 > api-token.key
FP=$(openssl x509 -in analyst.crt -outform der | shasum -a 256 | cut -d' ' -f1)
```

Deploy the agent with the dataset and keys sealed to the VM:

```sh
vcontainer deploy --session analyst docker.io/terryxhx/data-analyst@sha256:37531290c6973f0333b50f1f11c489cd688868c8ad6e5e5eefb9567d6bcbd3b6 -p 8443 -e TLS_CERT_SHA256=$FP --secret TICKETS=@tickets.csv --secret OPENROUTER_API_KEY=@openrouter.key --secret API_TOKEN=@api-token.key --secret TLS_CERT=@analyst.crt --secret TLS_KEY=@analyst.key
```

Write the policy that teammates verify against:

```sh
vcontainer verify --session analyst --export-policy analyst-policy.json
```

Give teammates the session's IP address (from `vcontainer ls`), `analyst-policy.json`, `ask.py` and `api-token.key` through a channel you trust. The policy must come from you, not from the service.

## Ask a question (a teammate)

With `vcontainer` installed, run `ask.py` with the custodian's policy, the token and the session's IP address. Replace `IP`:

```sh
python3 ask.py --policy analyst-policy.json --token api-token.key IP
```

It first runs `vcontainer verify` against the policy and stops if that fails. It reads the certificate's fingerprint from the launch that the policy pins, and for each question it compares the server's certificate with it before sending the token. Then it asks for questions until you enter an empty line or `quit`, or press Ctrl-D; to ask one question only, add it after `IP`.

Recorded with Docker on the synthetic dataset, with the verification step replaced by a stub, abbreviated:

```text
verified: docker.io/example/data-analyst@sha256:0000…
  certificate 0c37b0f08073ef8b…

question> Which team's first responses got slower in September, and by how much?
**Billing ops** is the team whose first responses got slower in September. ...
- **Size of the slowdown:** September was about 5.3 hours slower than August, which is roughly 87% higher. ...
- **Other teams:** Tier 1 and Tier 2 were essentially flat. Tier 1 went from 3.6 to 4.0 hours, and Tier 2 went from 2.8 to 3.1 hours.
```

Remove the session when you are done:

```sh
vcontainer rm --session analyst
```

## Try it with Docker

From this directory, with a certificate and token created as above, run the service on your machine:

```sh
docker build --platform linux/amd64 -t data-analyst .
docker run --rm --platform linux/amd64 -p 127.0.0.1:8443:8443 -e TLS_CERT_SHA256=$FP -v "$PWD/tickets.csv:/run/secrets/TICKETS:ro" -v "$PWD/openrouter.key:/run/secrets/OPENROUTER_API_KEY:ro" -v "$PWD/api-token.key:/run/secrets/API_TOKEN:ro" -v "$PWD/analyst.crt:/run/secrets/TLS_CERT:ro" -v "$PWD/analyst.key:/run/secrets/TLS_KEY:ro" data-analyst
```

There is no session to verify locally, so ask with `curl` and `-k`, which skips the certificate check. Use this only for a local test:

```sh
curl -k https://127.0.0.1:8443/ask -H "Authorization: Bearer $(cat api-token.key)" -d '{"question": "Why did first response times get worse in September?"}'
```

## Build your own image

The published image holds only Python and [agent.py](agent.py); the dataset and keys are sealed at deploy time. `vcontainer verify` shows which image ran, but not that it was built from this directory, so with the published image you trust its publisher. This example does not guarantee byte-for-byte reproducible builds, so your rebuild may have a different digest.

To run your own build, build and push this directory as in the review agent's [Build your own image](../pr-review-agent/README.md#build-your-own-image), with `data-analyst` in place of `pr-review-agent`, and deploy that reference in place of the published image. For a private Artifact Registry repository, `vcontainer deploy` seals your `gcloud` access token to the VM so it can pull the image.

## Using it from Slack

Teammates could ask in Slack instead. The agent would connect out to Slack with Socket Mode, answer mentions in one private channel, and record the workspace and channel IDs in the measured launch instead of a certificate fingerprint. It would open no port, and teammates would need no token or `ask.py`, only Slack; the custodian would instead seal Slack's bot and app tokens to the VM.

This changes who is trusted. Slack, and everyone in the channel, would see the questions and answers; raw rows still would not leave the VM. Teammates would trust the custodian's bot instead of verifying the VM before they ask; the custodian verifies it with `vcontainer verify`. Setting it up needs a Slack workspace where you may install apps, and Slack's SDK in the image. This example keeps HTTPS and `ask.py`, so that each teammate verifies the service themselves.

## Limits

- The custodian holds the TLS key and the token, so the custodian is trusted. This example protects the data from the cloud host and from the people who ask questions, not from the custodian.
- A client that connects and sends nothing holds the service for up to 30 seconds.
- After the VM restarts, run `vcontainer run` and deploy again; the certificate can stay. Give teammates the new IP address and the newly exported policy.
- The dataset is sealed whole in the launch, which is limited to 8 MB, so the CSV can be at most a few MB.
