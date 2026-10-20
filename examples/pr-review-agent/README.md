# Pull request review agent

An LLM agent that reviews one GitHub pull request at one commit. It prints the review or, given a GitHub token, posts it to the pull request. It is one file of Python with no dependencies, [agent.py](agent.py).

The model works through five tools and nothing else:

| Tool | What it returns |
|---|---|
| `list_changed_files` | the files the pull request changes |
| `get_diff` | the diff of one file, with the new line numbers |
| `read_file` | a text file of up to 1 MB at the reviewed commit |
| `list_dir` | a directory of the repository at the reviewed commit |
| `submit_review` | nothing: it ends the run with a summary and line comments |

The model has no shell, and no key is ever in its context. The agent fetches what the model asks for from the GitHub API and keeps it in memory; it never clones the repository. The image holds only Python and `agent.py`.

## Inputs

| Name | Kind | Meaning |
|---|---|---|
| `PR_URL` | environment | the pull request, `https://github.com/OWNER/REPO/pull/N` |
| `HEAD_SHA` | environment | the full commit to review; if the pull request's head is another commit, the agent stops |
| `MODEL` | environment, optional | an OpenRouter model ID (default `anthropic/claude-sonnet-5.5`) |
| `OPENROUTER_API_KEY` | secret | read from `/run/secrets/OPENROUTER_API_KEY` |
| `GITHUB_TOKEN` | secret, optional | a GitHub token; with it the agent posts the review to the pull request (see [below](#post-the-review-to-the-pull-request)) |

In vContainer, the environment variables are part of the measured launch, so `vcontainer verify` shows which pull request and commit the agent reviews.

Without `GITHUB_TOKEN`, the repository must be public, and GitHub allows 60 API requests per hour from one address; a small pull request takes about 10 to 20.

Exit codes: `0` the review was printed or posted, `2` a missing, malformed or unsupported input (such as a pull request with more files than GitHub lists, 300), `3` the pull request's head is not `HEAD_SHA`, or moved before the review was posted, `4` an error from GitHub or the model. Any other code means the agent crashed; the log shows why.

## Run it

Each block below is one command. Run them from this directory, with your OpenRouter key saved in `openrouter.key`.

Find the commit to review. Replace `OWNER`, `REPO` and `N`:

```sh
curl -s https://api.github.com/repos/OWNER/REPO/pulls/N | python3 -c 'import json, sys; print(json.load(sys.stdin)["head"]["sha"])'
```

To try the agent with Docker before deploying it, build the image:

```sh
docker build --platform linux/amd64 -t pr-review-agent .
```

Run it with the key mounted where vContainer puts secrets. Replace the pull request and `SHA`:

```sh
docker run --rm --platform linux/amd64 -v "$PWD/openrouter.key:/run/secrets/OPENROUTER_API_KEY:ro" -e PR_URL=https://github.com/OWNER/REPO/pull/N -e HEAD_SHA=SHA pr-review-agent
```

In vContainer, start a session and deploy the image. Replace `IMAGE` with a full image reference, such as one you pushed to Artifact Registry. The agent opens no port:

```sh
vcontainer run --session review
```

```sh
vcontainer deploy --session review IMAGE -e PR_URL=https://github.com/OWNER/REPO/pull/N -e HEAD_SHA=SHA --secret OPENROUTER_API_KEY=@openrouter.key
```

A review takes a minute or two. When `vcontainer verify` shows `state: Exited (exit code 0)`, read the review from the serial console:

```sh
vcontainer verify --session review
```

```sh
vcontainer logs --session review
```

Remove the session when you are done:

```sh
vcontainer rm --session review
```

## Post the review to the pull request

With a token in the secret `GITHUB_TOKEN`, the agent reads the repository with it, so the repository can be private, and posts the review to the pull request as a comment review. It never approves a pull request or requests changes. Comments on lines that the diff shows appear inline; the others go into the review's text. The agent keeps images out of the review, so that showing it loads nothing from another server; links and @mentions stay as the model wrote them. Before posting, the agent checks again that the pull request's head is `HEAD_SHA`.

With a token, the agent prints nothing of the repository: only the names of the tools it calls, the review's URL, and errors without details. In vContainer, pass `--quiet` to keep even those off the serial console.

If GitHub takes the review but the agent does not get the answer, the agent exits with `4`. Check the pull request before you run the agent again, or it posts a second review.

Create a [fine-grained personal access token](https://github.com/settings/personal-access-tokens/new) that reaches only the repository under review. If the repository belongs to an organization, choose the organization as **Resource owner**; the organization may have to approve the token before it works. Under **Repository access**, choose **Only select repositories** and the repository. Under **Permissions**, give **Contents** read-only and **Pull requests** read and write; GitHub adds **Metadata** read-only itself. Save the token in a file named `github.key` that holds only the token.

For a private repository, find the commit to review with `gh`. Replace `OWNER`, `REPO` and `N`:

```sh
gh pr view N --repo OWNER/REPO --json headRefOid -q .headRefOid
```

With Docker, mount the token next to the OpenRouter key:

```sh
docker run --rm --platform linux/amd64 -v "$PWD/openrouter.key:/run/secrets/OPENROUTER_API_KEY:ro" -v "$PWD/github.key:/run/secrets/GITHUB_TOKEN:ro" -e PR_URL=https://github.com/OWNER/REPO/pull/N -e HEAD_SHA=SHA pr-review-agent
```

In vContainer, add the token as a second secret and pass `--quiet`:

```sh
vcontainer deploy --session review IMAGE -e PR_URL=https://github.com/OWNER/REPO/pull/N -e HEAD_SHA=SHA --secret OPENROUTER_API_KEY=@openrouter.key --secret GITHUB_TOKEN=@github.key --quiet
```

`vcontainer verify` then lists both secret names, so whoever checks the session can see that the agent posts its review. The review is on the pull request once `verify` shows `state: Exited (exit code 0)`.
