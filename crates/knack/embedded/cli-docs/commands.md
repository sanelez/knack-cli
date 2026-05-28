## Commands

The CLI is a thin skill-store client. Authoring the SKILL.md folder happens
in chat with your agent (see `agent-integration` topic); the CLI publishes
the result. Knack runs in one of two modes, picked once at `knack init`:
self-host (your GitHub repo) or Knack Cloud. Most commands work the same
in both modes; the few that don't are flagged below.

### Setup

    knack init                                              # interactive picker (TTY only)
    knack init --self-host --github-repo <you>/<repo> \     # github mode, non-interactive
        [--visibility private|public] [--local-path DIR]
    knack init --cloud                                       # cloud mode, non-interactive

### Auth

    knack auth login    [--no-browser] [--account NAME]      # cloud only; no-op in github mode
    knack auth logout   [--account NAME]
    knack auth status                                        # mode-aware: shows backend + identity
    knack auth refresh                                       # cloud only; no-op in github mode

### Authoring

    knack create <slug> --name "..." [--description "..."] [--scope personal|team|public]
    knack validate <slug>
    knack publish <slug> [--from DIR] [--major|--minor|--patch] [--as-version X.Y.Z] [--dry-run]
    knack edit <slug> --name "..." --description "..." --scope ...        # cloud only

### Discovery + consumption

    knack list                                               # github: walks local clone; cloud: API
    knack search <query> [--sort recent|top|trending]        # github: local grep; cloud: marketplace
    knack pull <slug>[@<semver>] [--target DIR] [--global]
    knack pull @<owner>/<slug>[@<ver>]                       # external GitHub via Contents API
    knack pull @<owner>/<repo>:<slug>[@<ver>]                # external with custom repo
    knack diff <slug>@<a> <slug>@<b>

### Running + telemetry

    knack run <slug>[@<semver>] [--input PATH]... [--runtime TAG] [--agent-id ID]
    knack mark <run_id> succeeded|failed [--note "..."] [--reason "..."] [--output PATH]...

`--input` and `--output` are repeatable. In github mode, run telemetry
writes to `<your-clone>/runs/<YYYY-MM>/<YYYY-MM-DD>.jsonl` and is NOT
auto-committed (you commit + push the `runs/` tree yourself, or let the
next `knack publish` carry it).

### Interview (agent-driven authoring)

    knack interview start                                    # drops the interview skill into .claude/skills/
    knack interview save --session <id> --phase <p> --data <json>
    knack interview advance --session <id>
    knack interview status --session <id>
    knack interview resume --session <id>

### Folders, social, agent registration

    knack folder create|list|rename|delete|mv ...           # cloud only (personal + team scopes)
    knack feedback ...                                       # cloud only
    knack team ...                                           # cloud only
    knack fork <slug>                                        # cloud only
    knack rate <slug> <1-5>                                  # cloud only
    knack username claim <handle>                            # cloud only
    knack install [<agent>|--auto|--all]                     # register knack with the local agent runtime
    knack uninstall [--script]
    knack upgrade
    knack sync <slug>                                        # refresh per-skill agent shims

### Meta

    knack docs [<topic>]                                     # offline docs
    knack info                                               # canonical agent playbook
    knack introspect                                         # machine-readable command tree
    knack completions <shell>
    knack debug

Every command supports: `--json`, `--quiet`, `--no-color`, `--auth-token`,
`--account`.

Folders organize personal and team skills only. Folders are optional —
unfiled is a valid steady state — and every operation (create, rename,
move, delete) is reversible. The web workspace (Skill → Settings →
Folder section, plus the sidebar Folders list) and the CLI hit the
same `/folders` and `PATCH /skills/{id}` endpoints, so changes from
either surface appear in the other on next read.

### Typical agent-driven flow

    # one-time
    knack auth login

    # author the skill folder in chat with your agent (SKILL.md with
    # ## Intuition section, optional scripts/, assets/, references/), then:
    knack create month-end-close --name "Month-end close"
    knack publish month-end-close --from ./month-end-close

    # iterate
    knack pull month-end-close
    # edit files
    knack publish month-end-close            # auto-bumps patch

    # use the skill on real work
    knack run month-end-close --input ./october.xlsx
    knack mark <run_id> succeeded
