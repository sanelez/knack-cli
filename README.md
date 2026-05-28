<img width="1600" height="800" alt="Hero (1)" src="https://github.com/user-attachments/assets/b4b9a465-6944-4eea-a381-8817f030931c" />

# Knack

A CLI for authoring, validating, versioning, running, and observing agent skills.

Knack turns the workflows you do over and over into Anthropic Skills:
structured folders of instructions, examples, tests, and metadata that any
agent (Claude, Cursor, Codex, Cowork) can load. The artifact is real and
inspectable. Every version is immutable. Every run is logged. When a skill
misses an edge case, you flag it, the skill gets a new rule, and the next
run is sharper than the last.

## What you ship

```
skills/email-triage/
├── SKILL.md            # the playbook the agent loads
├── meta.knack.yaml     # id, name, slug, author, version, description
├── intuition.md        # the edge cases and judgment calls
├── examples/           # input/output pairs from real past work
├── scripts/            # optional helper scripts
├── assets/             # optional static files
├── references/         # optional reference material
└── tests/              # optional assertions that run pre-publish
```

This is the open [Anthropic Skills](https://github.com/anthropics/skills)
format. Portable across agents. Plain text. Diffable.

## Install

```bash
curl -fsSL https://knack.ai/install | sh
```

Windows (PowerShell):

```powershell
irm https://knack.ai/install.ps1 | iex
```

Or in any Claude Code, Cursor, or Codex session, just say "install knack."

## Two modes, one CLI

**Self-host (GitHub).** Skills live in a private GitHub repo under your
account. No third-party account, no telemetry leaves your machine, no
caps. Publishing is a git commit, a per-skill tag, and a push.

```bash
knack init --self-host --github-repo <you>/<your-skills-repo>
```

**Knack Cloud.** Zero setup. Public marketplace and team features
(sharing, roles, audit log, SSO) live here. Free tier at
[getknack.ai](https://getknack.ai).

```bash
knack init --cloud
knack auth login
```

Same commands. Same skill folder format. The backend just decides where
versions and run logs go. Pick once at `knack init`; switch later by
re-running it.

## What works in self-host mode

The full lifecycle, with no cloud round-trip:

| Command | What it does in self-host |
|---|---|
| `knack init --self-host` | Creates the GitHub repo (if missing), clones locally, scaffolds layout, pushes the initial commit |
| `knack auth status` | Shows backend, repo, local clone, resolved `gh` user |
| `knack auth login` | Friendly no-op (you don't need to sign into anything) |
| `knack create <slug>` | Scaffolds a new skill folder in your clone |
| `knack validate <slug>` | Pre-flight checks `SKILL.md` + `meta.knack.yaml` + `tests/` |
| `knack publish <slug>` | Bumps version in `meta.knack.yaml`, commits, tags `<slug>/v<X.Y.Z>`, pushes |
| `knack list` | Walks your local clone, shows every skill + current version |
| `knack search <q>` | Greps the local clone for matches in slug / description / SKILL.md |
| `knack pull <slug>` | Resolves the latest tag, writes the skill folder into your workspace |
| `knack pull <slug>@<ver>` | Pulls a specific historical version |
| `knack pull @other-user/<slug>` | Fetches from any public `<owner>/knack-skills` repo via the GitHub Contents API |
| `knack pull @other-user/<repo>:<slug>@<ver>` | Full external spec: owner, repo, slug, optional version |
| `knack run <slug> --input ... --input ...` | Registers a run, writes a `started` event to local JSONL with all inputs |
| `knack mark <run-id> succeeded --output ... --output ... --note ...` | Closes the loop with outputs, note, and a computed `duration_ms` |

## What lives only in cloud mode

Be honest about the line. These either require knack.ai's server or only
make sense once multiple users are involved:

- `knack feedback` — pings the skill author with a bug report
- `knack team` — team membership, roles, invitations
- `knack fork` — server-side copy of someone else's marketplace skill
- `knack rate` — 5-star rating on a public marketplace skill
- `knack username` — claim a marketplace handle for public publishing
- Cross-repo `knack search` — only your local clone is searched in self-host mode
- Server-side run rollups and per-skill stats dashboards

Calling these in self-host mode prints a `REQUIRES_CLOUD` message and
exits cleanly.

## Run telemetry schema

Every `knack run` and `knack mark` appends one line to
`<your-repo>/runs/<YYYY-MM>/<YYYY-MM-DD>.jsonl`. Same schema in both
modes; in cloud mode it also lands server-side for the rollups.

```jsonc
// `knack run email-triage --input ./today.eml --runtime claude-code`
{
  "event": "started",
  "run_id": "2efecd46-...",
  "skill": "email-triage",
  "version": "0.2.1",
  "agent": "claude-code",
  "inputs": ["./today.eml"],
  "status": "started",
  "at": "2026-05-27T18:42:11Z"
}

// `knack mark 2efecd46-... succeeded --output ./out/triaged.csv --note "clean"`
{
  "event": "marked",
  "run_id": "2efecd46-...",
  "skill": "email-triage",
  "version": "0.2.1",
  "agent": "claude-code",
  "inputs": ["./today.eml"],
  "outputs": ["./out/triaged.csv"],
  "status": "succeeded",
  "note": "clean",
  "at": "2026-05-27T18:42:16Z"
}
```

`knack mark` returns a `RunSnapshot` that also includes `duration_ms`
(computed from the two events' timestamps). Fields are documented in
[`crates/knack-backend-github/src/runs.rs`](crates/knack-backend-github/src/runs.rs).

**Push policy.** Run events are NOT auto-committed or auto-pushed. They
sit locally in the working tree. Commit and push the `runs/` directory
yourself when you want them on the remote, or let the next
`knack publish <skill>` ride along (it commits the whole working tree).

## The iteration loop

1. Author. `knack create my-skill` scaffolds. Your agent runs the 6-phase
   interview and fills in `SKILL.md` + `intuition.md` + `examples/`.
2. Validate. `knack validate my-skill` catches schema mistakes locally
   before you burn a version number.
3. Publish. `knack publish my-skill` bumps the version, commits, tags,
   pushes. Tag is `my-skill/v<X.Y.Z>`. Immutable.
4. Run. `knack run my-skill --input ...`. The agent does the actual work
   using SKILL.md as its playbook. CLI generates the `run_id` and writes
   the `started` event.
5. Mark. `knack mark <run-id> succeeded --output ...` (or
   `failed --reason "..."`). The note text feeds back into the next
   interview pass.
6. Bump. When a miss matters, edit `intuition.md`, then `knack publish`
   again. Version goes from 0.1.1 to 0.1.2. The git log is the history.

## For agents loading this README

Operating surface, when you're driving the CLI on a user's behalf:

- Binary is `knack`. `knack --help` for the full tree; `knack introspect --json`
  for a machine-readable command catalog.
- User's backend mode is at `~/.knack/config.yaml`. Read it before anything
  else: `mode: github` skips cloud auth; `mode: cloud` requires
  `knack auth login`.
- Skill folders are at `<workspace>/skills/<slug>/`. Required:
  `SKILL.md` (with frontmatter `name`, `description`) and `meta.knack.yaml`
  (with `id`, `name`, `slug`, `author`, `version`). Optional:
  `intuition.md`, `examples/`, `scripts/`, `assets/`, `references/`,
  `tests/`.
- The 6-phase interview skill is embedded in the binary. Start it with
  `knack interview start`. The CLI writes the skill into
  `<cwd>/.claude/skills/knack-interview/` and returns a session id you pass
  to subsequent `knack interview save / advance` calls.
- Pre-flight validation: `knack validate <slug>` returns a structured
  issues list. Fix and retry locally before paying any publish round-trip.
- `--input` on `knack run` and `--output` on `knack mark` are both
  repeatable. Pass each file separately.
- Every command supports `--json`. Stdout is JSON; stderr is chatter.
- Exit codes are stable: `0` success, `1` user, `2` auth, `3` network,
  `4` conflict, `5` plan limit, `64` usage, `70` internal. See
  `knack docs exit-codes` for the full list.
- The canonical agent playbook is `knack info`. It fetches
  `getknack.ai/agent.txt` and falls back to the embedded copy on offline.

## What's in this repo

```
crates/knack/                  the CLI binary (Rust)
crates/knack-types/            shared wire-format types (on crates.io)
crates/knack-backend-github/   self-host implementation (libgit2 + gh CLI + GitHub Contents API)
skills/interview/              the 6-phase interview skill the agent loads
skills/installer/              the install skill agents load on first ask
install.sh / install.ps1       curl|sh / irm|iex installer
.github/workflows/             CI + release matrix (macos x86_64, linux x86_64-musl, linux aarch64-musl, windows x86_64)
```

## Contributing

Bug fixes, typo fixes, and documentation improvements are welcome. Feature
PRs are kept narrow. Open a Discussion first so we can talk through
direction before code lands. See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

MIT. See [LICENSE](LICENSE).
