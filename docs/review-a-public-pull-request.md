# Review a public pull request

The [pull request review agent](../examples/pr-review-agent) works through tools. It reviews one GitHub pull request at one commit: the model reads the diff and other text files of the repository, including unchanged ones, through five tools, with no shell, and finishes with a review. Its image is on Docker Hub, so nothing needs to be built. Here it reviews [pull request #1](https://github.com/haoxxu/vContainer-review-demo/pull/1) of a small demo repository, which adds a function with three deliberate bugs.

Complete [setup](install.md) first. Save your OpenRouter API key in a file named `openrouter.key` that holds only the key, then start a session:

```sh
vcontainer run --session public-review
```

Deploy the published v3 agent, pinned by digest. The pull request and its commit are environment variables, so they are part of the measured launch; if the pull request has moved to another commit, the agent stops without reviewing it. The agent opens no port:

```sh
vcontainer deploy --session public-review docker.io/terryxhx/pr-review-agent@sha256:23c5696ea6a1544e3d82025d91c3ad74ecce14638abcaa8c56c6ed85933e2aad \
  -e PR_URL=https://github.com/haoxxu/vContainer-review-demo/pull/1 \
  -e HEAD_SHA=688c61413e4e9bf830a58bab3e344d25582b8a33 \
  --secret OPENROUTER_API_KEY=@openrouter.key
```

With `OUTPUT` unset, the agent prints this public repository's review to the serial console. A `GITHUB_TOKEN` alone no longer requests posting; posting requires both `OUTPUT=post` and a token.

The deploy returns once the container runs, after about 15 seconds. The review takes a minute or two; then verify the session again:

```sh
vcontainer verify --session public-review
```

Expected output, abbreviated (not recorded with v3; the interactive terminal presents these fields in a receipt):

```console
public-review: VERIFIED
  ...
  ✓ launch measured before pull: docker.io/terryxhx/pr-review-agent@sha256:23c5696e…
  ✓ container sealed: manifest sha256:<selected-linux-amd64-manifest>
  state: Exited (exit code 0)
  container: docker.io/terryxhx/pr-review-agent@sha256:23c5696e… env=["PR_URL=https://github.com/haoxxu/vContainer-review-demo/pull/1", "HEAD_SHA=688c61413e4e9bf830a58bab3e344d25582b8a33"] secrets={"OPENROUTER_API_KEY"}
```

The manifest placeholder above is the selected `linux/amd64` image manifest; it can differ from the pinned digest if that digest names an image index.

The evidence names the agent's image, the pull request and the commit, and the exit code is part of the quote. Read the review from the serial console; `sed` skips the boot messages before it:

```sh
vcontainer logs --session public-review | sed -n '/^pr-review-agent:/,$p'
```

<details>
<summary>Recorded review output (from an earlier image, v2)</summary>

```console
pr-review-agent: haoxxu/vContainer-review-demo#1 at 688c61413e4e, model anthropic/claude-sonnet-5.5
  tool get_diff {"path": "notes/listing.py"}
  tool list_dir {"path": "notes"}
  tool read_file {"path": "notes/cache.py"}
  tool read_file {"path": "notes/db.py"}
  tool get_diff {"path": "notes/__init__.py"}

review of haoxxu/vContainer-review-demo#1 at 688c61413e4e: 4 comments

The new `list_notes` has four defects. Two of them make it wrong on every call, and one is a SQL injection. I read the diff and `notes/cache.py` and `notes/db.py`. I did not run the code.

notes/listing.py:16
  `Cache.get` raises KeyError on a miss or an expired entry. It never returns None, as its docstring in cache.py says. So the first call for any key raises KeyError and `list_notes` never reaches the database. Wrap the lookup in try/except KeyError and fall through to the query.
...
tokens: 9651 prompt, 1067 completion
vcontainer: container exited with 0
```

In this run the agent found the three planted bugs and further problems with the cache. It found the cache bug by reading `notes/cache.py`, a file the pull request does not change. The review varies from run to run.

</details>

In v3, each model reply is limited to 8,000 tokens. Once OpenRouter reports that a run has used 1,000,000 tokens, the next turn is the last; this is a stopping point, not a hard cap, because the last turn adds tokens. A run also stops after at most 30 model turns, with exit code `4` if no review was submitted.

The OpenRouter key is sealed to the VM, and the model never sees it: the agent calls OpenRouter itself and gives the model only its tools. Everything else here is public: the repository, the launch and the review on the serial console. OpenRouter and the model's provider see the code under review. To let someone else check the deployment, follow [Verify and share](verify-and-share.md) before cleanup. Otherwise, remove the VM to stop billing:

```sh
vcontainer rm --session public-review
```

