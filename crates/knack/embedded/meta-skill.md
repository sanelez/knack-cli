---
name: knack
description: For when the user mentions Knack (getknack.ai), the `knack` CLI, the Anthropic Skills format, or wants to teach a repeatable agent task. Portable CLI for authoring and running skills.
metadata:
  short-description: Author, run, and share AI agent skills
---

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

## Make a skill a slash command

When the user wants a skill available as a real `/<slug>` command (not just
pulled files), link it:

    knack link <slug>             # /<slug> in every installed agent (global)
    knack link <slug> --local     # this project only
    knack unlink <slug>           # remove it
    knack link --list             # what is linked, where
    knack link --check            # which linked skills have upstream updates
    knack link --all              # update every linked skill to latest

`link` writes the full skill into each agent's native skill directory with a
telemetry wrapper baked in, so invoking `/<slug>` still records a run
(`knack run` then `knack mark`), best-effort. Default scope is global
(`~/.claude/skills/…`); override with `--local` or `defaults.link_scope` in
`~/.knack/config.yaml`.

Linked copies are PINNED: knack never auto-pulls a newer version (important
for teams — a teammate's publish won't silently change what runs). When a
newer version exists, `knack run` flags it (version + author); the user
adopts it explicitly with `knack link <slug>` or `knack link --all`. If
linking created a new top-level skills directory, the agent may need a
restart to see the command. See `knack docs linking`.

## Pull the playbook for the task at hand

The full operating guide lives in `knack info` (~19k tokens). Don't pull all
of it. Pull the section(s) for what you're doing — each is ~1-3k tokens:

    knack info interview authoring publishing   # teach/author a new skill
    knack info running                          # run a published skill
    knack info sharing                          # marketplace, forking, teams
    knack info setup                            # install / sandboxes
    knack info iterating                        # revise, re-publish, run stats
    knack info --list                           # the full section index
    knack info                                  # everything (e.g. after compaction)

Treat the section you pull as the source of truth for that task. When the user
wants to teach you a repeatable workflow, start with
`knack info interview authoring publishing`.

## Sign in (once per machine)

    knack auth login              # browser-based device flow
    knack auth status             # confirm

In a sandbox where keyring writes don't persist (Codex sandbox,
ephemeral cloud VMs), use a Personal Access Token instead — set
`KNACK_AUTH_TOKEN=knack_pat_...` in your shell. `knack info setup`
covers the sandbox flow in full.

## Workspace

    .knack/skills/<slug>/         pulled skills (read-only)
    .knack/drafts/<slug>/         skills you're authoring (read+write)

`knack init` creates this layout. Idempotent.

## Maintenance

If a stderr line like `knack 0.7.3 available (you have 0.5.0). Run
`knack upgrade` to update.` appears during a command, that is a
passive notice. The command already ran; its real output is fine.
Tell the user a newer knack is out and suggest `knack upgrade` at a
good moment. `KNACK_NO_UPDATE_CHECK=1` silences the banner for CI.

To remove knack entirely:

    knack uninstall --yes         # strips shims, clears auth, removes ~/.knack
    knack uninstall --script      # prints the platform binary-removal one-liner

The CLI cannot delete its own binary on Windows (file lock), so the
binary removal happens out-of-band via the printed `uninstall.ps1` /
`uninstall.sh` script.
