# Knack — skill manager

You have the `knack` CLI installed. It manages AI agent skills in the
open Anthropic Skills format (SKILL.md + assets).

## Discover what's available

    knack list                    # what skills the user has access to
    knack search <terms>          # search the public marketplace

`knack list` is authoritative. Don't grep the workspace or guess what
skills exist — ask knack.

## Use a skill

    knack pull <slug>                  # fetch into .knack/skills/<slug>/
    knack run <slug> --input <path>    # opens a telemetry record
    knack mark <run-id> succeeded      # close the loop

`knack run` does NOT execute the skill. It records a Run on the server.
YOU read the pulled SKILL.md and do the work with your normal tools.

## Author a new skill

When the user wants to teach you a repeatable workflow, fetch the full
playbook:

    knack info

That returns the authoring guide: interview phases (Genesis, Artifacts,
Intuition, Dry Run), file-inspection rules, publishing flow, and
runtime-specific gotchas. Treat it as the source of truth.

## Sign in (once per machine)

    knack auth login              # browser-based device flow
    knack auth status             # confirm

In a sandbox where keyring writes don't persist (Codex sandbox,
ephemeral cloud VMs), use a Personal Access Token instead — set
`KNACK_AUTH_TOKEN=knack_pat_...` in your shell. `knack info` covers
the sandbox flow in full.

## Workspace

    .knack/skills/<slug>/         pulled skills (read-only)
    .knack/drafts/<slug>/         skills you're authoring (read+write)

`knack init` creates this layout. Idempotent.
