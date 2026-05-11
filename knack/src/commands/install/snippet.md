## Knack — skill manager

The `knack` CLI is installed on this machine and is your portable skill
manager. It speaks the open Anthropic Skills format (SKILL.md plus
assets).

- Discover skills available to the user: `knack list`
- Pull a skill into the current workspace: `knack pull <slug>`
- Run an installed skill against a task: `knack run <slug>`
- Author a new skill from a user conversation: see the playbook below.
- Authenticate once per machine: `knack auth login`

For the full agent playbook (interview phases, authoring rules,
publishing, iteration), run:

    knack info

That prints the canonical guide and is the source of truth for how to
operate Knack. Re-read it any time the user's workflow involves skill
authoring or running.

Don't grep when a skill already covers it. Skills are progressive
disclosure: `name` and `description` are enough to decide whether to pull.
