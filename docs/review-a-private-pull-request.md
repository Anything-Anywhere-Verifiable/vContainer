# Review a private pull request

The [pull request review agent](../examples/pr-review-agent) can review a pull request in a private repository and post the review there, while the host sees neither the code the agent reads, the token nor the review. You copy the demo repository into a private one, give the agent a GitHub token for that repository only, and keep the container's output off the serial console. The demo's code is public anyway; with your own private repository, the same steps keep its code from the host.

Complete [vContainer setup](install.md) first. Have `gcloud` authenticated with the project selected and save your OpenRouter API key in `openrouter.key`. Run the commands below from the repository root. This guide creates a private copy of the public demo repository; to review your own repository, use its pull request URL and exact head commit instead.

This example also uses the [GitHub CLI](https://cli.github.com), `gh`, signed in with `gh auth login` to the account that will own the copy.

Copy the demo repository, with its history, into a new private repository named `review-demo`. "Use this template" does not work here: the branches of a template copy have unrelated histories, so GitHub cannot open a pull request between them. Create the empty repository:

```sh
gh repo create review-demo --private
```

Let `git` push to GitHub over HTTPS with your `gh` login (once per machine). If you push over SSH instead, skip this and push to `git@github.com:OWNER/review-demo.git` below:

```sh
gh auth setup-git
```

Clone the demo repository without a working tree:

```sh
git clone --bare https://github.com/haoxxu/vContainer-review-demo.git
```

Push every branch to your copy. Replace `OWNER` with your GitHub account, here and below:

```sh
git -C vContainer-review-demo.git push --mirror https://github.com/OWNER/review-demo.git
```

Open the pull request in your copy:

```sh
gh pr create --repo OWNER/review-demo --base main --head feat/list-notes --title "feat: add paginated note listing" --body "Adds list_notes, which lists the notes of one owner, a page at a time."
```

Create a [fine-grained personal access token](https://github.com/settings/personal-access-tokens/new) for this repository only. Under **Repository access**, choose **Only select repositories** and `review-demo`. Under **Permissions**, give **Contents** read-only and **Pull requests** read and write. Save the token in a file named `github.key` that holds only the token. Then start a session:

```sh
vcontainer run --session private-review
```

Deploy the published v3 review-agent image. The copy keeps the demo's commits, so `HEAD_SHA` does not change. With `OUTPUT=post` and `GITHUB_TOKEN`, the agent posts its review to the pull request and prints nothing of the repository, and `--quiet` keeps even that off the serial console:

```sh
vcontainer deploy --session private-review docker.io/terryxhx/pr-review-agent@sha256:23c5696ea6a1544e3d82025d91c3ad74ecce14638abcaa8c56c6ed85933e2aad \
  -e PR_URL=https://github.com/OWNER/review-demo/pull/1 \
  -e HEAD_SHA=688c61413e4e9bf830a58bab3e344d25582b8a33 \
  -e OUTPUT=post \
  --secret OPENROUTER_API_KEY=@openrouter.key \
  --secret GITHUB_TOKEN=@github.key \
  --quiet
```

A token alone does not post. With `OUTPUT` unset, v3 prints public reviews but refuses to print a private repository's review. Printing a private review requires an explicit `OUTPUT=print`; in vContainer, the host can read serial-console output unless it is suppressed with `--quiet`. This walkthrough uses `OUTPUT=post` so the review goes to GitHub.

After a minute or two, verify the session:

```sh
vcontainer verify --session private-review
```

Expected output, abbreviated (not recorded with v3; the interactive terminal presents these fields in a receipt):

```console
private-review: VERIFIED
  ...
  state: Exited (exit code 0)
  container: docker.io/terryxhx/pr-review-agent@sha256:23c5696e… env=["PR_URL=https://github.com/OWNER/review-demo/pull/1", "HEAD_SHA=688c61413e4e9bf830a58bab3e344d25582b8a33", "OUTPUT=post"] secrets={"GITHUB_TOKEN", "OPENROUTER_API_KEY"}
```

`OUTPUT=post` and both secret names are part of the evidence, so whoever checks the session can see that the agent had a GitHub token and was configured to post its review. The serial console shows the container start and exit, and nothing in between:

```sh
vcontainer logs --session private-review | tail -n 2
```

Expected final serial-console lines:

```console
vcontainer: container started: python3 -u /app/agent.py
vcontainer: container exited with 0
```

On success, open the pull request to read the review. The agent posts a comment review of commit `688c6141` under the account that owns the token, with inline comments on the lines it discusses:

```sh
gh pr view 1 --repo OWNER/review-demo --web
```

In v3, each model reply is limited to 8,000 tokens. After OpenRouter reports 1,000,000 tokens used, the next turn is the last and can add more tokens; this is not a hard spending cap. A run also stops after at most 30 model turns, with exit code `4` if no review was submitted.

What this run keeps from whom:

- The host cannot read the code the agent reads, the token or the review. The token is sealed to the VM, the agent keeps the code in the VM's encrypted memory, its traffic to GitHub and OpenRouter is HTTPS, the container's output stays off the serial console, and the VM has no SSH or exec.
- The launch is public: the image, the commit and `PR_URL`, which names your repository.
- GitHub, OpenRouter and the model's provider see the code. So does anything your GitHub settings let read the repository, such as an app or automatic Copilot code review; vContainer does not change those settings. The host sees which servers the VM connects to and how much data it exchanges with them.
- The image is the one we publish, pinned by digest. `vcontainer verify` shows which image ran, but not that it was built from [examples/pr-review-agent](../examples/pr-review-agent), so you trust its publisher. To run your own build, follow [Build your own image](../examples/pr-review-agent/README.md#build-your-own-image) and deploy that reference instead.

To share this deployment for independent verification before cleanup, follow [Verify and share](verify-and-share.md), using `private-review` as the session name.

Remove the session when you are done. The token and the private repository stay until you delete them on GitHub:

```sh
vcontainer rm --session private-review
```
