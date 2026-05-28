Authentication in knack depends on your backend mode.

    knack auth status

…tells you which mode you're in and what the relevant identity is.

## Self-host (github) mode

You don't need to "sign into knack" at all. The skill repo lives in your
own GitHub account; auth happens via the `gh` CLI's existing credential
helper. Verify with:

    gh auth status

If gh isn't authenticated, run `gh auth login` once. After that, knack
in github mode never asks for credentials again.

`knack auth login` in github mode is a friendly no-op (it prints a note
explaining that self-host doesn't need a knack.ai sign-in and exits 0).
`knack auth status` shows the configured backend, repo, local clone, and
resolved `gh` user.

## Knack Cloud mode

    knack auth login

Opens your default browser to a Clerk-gated approval page. After you
sign in and click Approve, the CLI receives a Personal Access Token
(PAT) and stores it in `~/.knack/auth.json`.

### Headless / CI

    knack auth login --no-browser

Prints the verification URL to stderr instead of opening a browser.
Useful for SSH sessions and containers.

### Stateless sandboxes (Cowork, hard subprocess timeouts)

    knack auth login --start              # prints device_code + URL, exits
    # human approves in their own browser
    knack auth login --poll <device_code> # repeat until "approved"

The `--start` / `--poll` flow lets long-lived browser approvals span
multiple short-lived tool calls without keeping a subprocess alive
across the full TTL.

### Service accounts (CI)

Set `KNACK_AUTH_TOKEN` directly:

    export KNACK_AUTH_TOKEN=knack_pat_xxx
    knack list

Tokens are scoped per-user. Issue them at
`getknack.ai/app/settings#cli-tokens`.

### Multiple accounts

    knack auth login --account work
    knack auth login --account personal
    knack --account work list

### Logout

    knack auth logout              # current account
    knack auth logout --account work

## Switching between modes

Your backend mode is in `~/.knack/config.yaml`. To switch, re-run
`knack init`:

    knack init --cloud         # switch to Knack Cloud
    knack init --self-host \   # switch to GitHub self-host
        --github-repo <you>/<repo> --visibility private
