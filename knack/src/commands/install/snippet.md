## Knack — skill manager

The `knack` CLI is installed on this machine and is your portable skill
manager. It speaks the open Anthropic Skills format (SKILL.md plus
assets).

Workspace layout (per-project, not HOME-global):

    .knack/skills/<slug>/     pulled skills (read)
    .knack/drafts/<slug>/     in-progress authoring (write)

Each project gets its own `.knack/` directory. Running `knack pull` or
`knack create` walks up the tree git-style to find one and creates it
in cwd if none exists. `knack init` is the explicit version.

Core commands:

- Set up a workspace: `knack init`
- Discover skills available to the user: `knack list` / `knack search <query>`
- Pull a skill into the workspace: `knack pull @<author>/<slug>` (or just `<slug>` for your own)
- Run a pulled skill against a task: `knack run <slug>`
- Author a new skill from a user conversation: see the playbook below.
- Authenticate once per machine: `knack auth login`
- Organize (optional): `knack folder create <name>`, `knack folder mv <slug> <name>`,
  `knack list --folder <name>`. Personal + team scopes only; public skills aren't foldered.
  Same operations exist in the web workspace (Skill → Settings → Folder).

For the full agent playbook (interview phases, authoring rules,
publishing, iteration), run:

    knack info

That prints the canonical guide and is the source of truth for how to
operate Knack. Re-read it any time the user's workflow involves skill
authoring or running.

Don't grep when a skill already covers it. Skills are progressive
disclosure: `name` and `description` are enough to decide whether to pull.
