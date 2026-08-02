---
name: add-or-update-slash-command
description: Workflow command scaffold for add-or-update-slash-command in grok-build.
allowed_tools: ["Bash", "Read", "Write", "Grep", "Glob"]
---

# /add-or-update-slash-command

Use this workflow when working on **add-or-update-slash-command** in `grok-build`.

## Goal

Introduces a new slash command or updates an existing one in the TUI, involving files under crates/codegen/xai-grok-pager/src/slash/commands/ and updating documentation.

## Common Files

- `crates/codegen/xai-grok-pager/src/slash/commands/*.rs`
- `crates/codegen/xai-grok-pager/src/slash/mod.rs`
- `crates/codegen/xai-grok-pager/src/slash/registry.rs`
- `crates/codegen/xai-grok-pager/docs/user-guide/04-slash-commands.md`
- `crates/codegen/xai-grok-pager/docs/tutorial/05-slash-commands.md`

## Suggested Sequence

1. Understand the current state and failure mode before editing.
2. Make the smallest coherent change that satisfies the workflow goal.
3. Run the most relevant verification for touched files.
4. Summarize what changed and what still needs review.

## Typical Commit Signals

- Create or update a file in crates/codegen/xai-grok-pager/src/slash/commands/<command>.rs
- Update mod.rs or registry.rs to register the command
- Update user documentation in crates/codegen/xai-grok-pager/docs/user-guide/04-slash-commands.md or tutorial/05-slash-commands.md
- Optionally add or update tests

## Notes

- Treat this as a scaffold, not a hard-coded script.
- Update the command if the workflow evolves materially.