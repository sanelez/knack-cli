# Linking skills as slash commands

`knack link <slug>` installs a published skill as a native `/<slug>` slash
command in every AI agent registered on this machine (the agents you ran
`knack install` for). It downloads the skill bundle and writes the whole
folder into each agent's native skill directory, so the command is
self-contained and works immediately.

```
knack link monthly-close            # /monthly-close in every installed agent
knack link monthly-close@1.2.0      # pin a specific version
knack link monthly-close --agent claude   # one agent only
knack link --list                   # what is linked, where
knack unlink monthly-close          # remove it again
```

This is different from `knack pull`, which only drops files into
`.knack/skills/` (a directory agents do not scan). Use `pull` to fetch
files for editing; use `link` when you want the skill available as a
command.

## Telemetry is preserved

A linked skill keeps knack's run loop. The installed `SKILL.md` is wrapped
so the agent records a run with `knack run <slug>` before doing the work
and closes it with `knack mark <run_id> succeeded` afterward. Telemetry is
best-effort: if `knack run` fails (offline or signed out) the skill still
runs, so linking never makes a skill unusable.

## Scope: global vs local

- `--global` (the default) writes to your HOME agent dirs
  (`~/.claude/skills/<slug>/`, `~/.agents/skills/<slug>/` for Codex, and so
  on). The command works in every project.
- `--local` writes to the workspace agent dirs (`.claude/skills/<slug>/`,
  `.agents/skills/<slug>/`, …). The command exists only in that project.

Change the no-flag default in `~/.knack/config.yaml`:

```yaml
defaults:
  link_scope: project   # or: home (the built-in default)
```

Precedence note: Claude Code resolves a personal (global) skill ahead of a
project one with the same name. If you link the same slug both globally and
locally, the global copy wins; `knack link` prints a note when this
applies.

## Picking it up

Adding or removing a skill inside an agent skill directory that already
exists is detected live. The one exception: if the top-level skills
directory did not exist when the agent session started (for example the
first time you ever link globally and `~/.claude/skills/` is created),
restart the agent so it watches the new directory. `knack link` prints a
reminder when it had to create that directory.

## Updating, and team workflows

A linked skill is **pinned** to the version you linked. knack never pulls a
newer version on its own. This is deliberate and matters most for teams: when
a teammate publishes a new version of a shared skill, your `/<slug>` command
keeps running exactly what you linked until you choose to update. No silent
behavior changes mid-task.

What you get instead is a **flag**. When you use the skill, the wrapper runs
`knack run <slug>`, which checks whether a newer version was published and, if
so, tells you the new version and who authored it:

```
update available: `monthly-close` is linked at 1.0.0 but 1.1.0 (by alice) is
published. Run `knack link monthly-close` to update (nothing changed automatically).
```

To see this across everything you've linked, without changing anything:

```
knack link --check        # which linked skills have upstream updates, and by whom
```

When you decide to adopt updates (an explicit, user-initiated pull):

```
knack link monthly-close  # update one skill to the latest
knack link --all          # update every linked skill to the latest
```

Silence the per-run flag with `KNACK_NO_LINK_UPDATE_CHECK=1` if you prefer.

## Removing

`knack unlink <slug>` removes the command from every agent at the chosen
scope. `knack uninstall` and `knack sync --purge` also sweep linked skills
along with everything else knack installed. Removal is sigil-protected: a
skill folder you authored yourself (no knack marker) is never deleted.
