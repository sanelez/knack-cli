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

`knack auth login` in github mode probes `gh api user` and reports
whether gh is installed and authenticated. No Knack token is minted —
the `gh` credential is the only thing every subsequent command uses.
Exit is always 0 (informational), but the JSON envelope carries
`needs_action: true` when gh isn't set up so an agent driving the CLI
can branch on it. `knack auth status` shows the configured backend,
repo, local clone, and resolved `gh` user.

## Knack Cloud mode

    knack auth login

Opens your default browser to a Clerk-gated approval page. After you
sign in and click Approve, the CLI receives a Personal Access Token
(PAT) and stores it in `~/.knack/auth.json`.

PATs **default to a 90-day expiry**. Override with
`--expires-in-days N` (1..730) or opt out entirely with
`--never-expires` — but a leaked never-expiring PAT works forever, so
reach for it only on unattended CI where the token lives in a vault and
rotation is impractical. The CLI surfaces an `AuthRequired` one day
before expiry so a re-login is rarely a surprise.

### Token scopes

PATs default to `--scope full`, which reproduces every pre-scopes
behavior. Pass `--scope read` to mint a **read-only PAT**:

    knack auth login --scope read --label "ci-stats-reader"

Read-scoped PATs authenticate fine on GET routes (`/skills`,
`/runs/overview`, `/runs/by-skill/...`, etc.) but are rejected with
`403 read_only_token_cannot_write` on every write route, with three
deliberate exceptions:

- `POST /runs/{id}/mark` — `knack mark` is a verdict on existing work,
  not authoring. CI agents that read run state and verdict need this.
- `POST /feedback/threads` + `POST /threads/{id}/messages` — feedback
  is user-initiated speech, not state mutation on a skill.
- `DELETE /me/cli-tokens/{id}` — revoking your own token is harmless
  cleanup and never needs full scope.

Use `--scope read` for any token that lives in a vault and only needs
to poll telemetry. The blast radius of a leak shrinks from "everything
the user can do" to "everything the user can read."

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
