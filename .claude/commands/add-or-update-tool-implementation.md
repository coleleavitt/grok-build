---
name: add-or-update-tool-implementation
description: Workflow command scaffold for add-or-update-tool-implementation in grok-build.
allowed_tools: ["Bash", "Read", "Write", "Grep", "Glob"]
---

# /add-or-update-tool-implementation

Use this workflow when working on **add-or-update-tool-implementation** in `grok-build`.

## Goal

Adds a new tool implementation or updates an existing one in the grok-build tool suite, often with new files or changes under crates/codegen/xai-grok-tools/src/implementations/grok_build/* and related test or type files.

## Common Files

- `crates/codegen/xai-grok-tools/src/implementations/grok_build/*`
- `crates/codegen/xai-grok-tools/src/implementations/grok_build/mod.rs`
- `crates/codegen/xai-grok-tools/src/types/*.rs`
- `crates/codegen/xai-grok-tools/src/registry/types.rs`

## Suggested Sequence

1. Understand the current state and failure mode before editing.
2. Make the smallest coherent change that satisfies the workflow goal.
3. Run the most relevant verification for touched files.
4. Summarize what changed and what still needs review.

## Typical Commit Signals

- Create or update files in crates/codegen/xai-grok-tools/src/implementations/grok_build/<tool_name>/
- Update corresponding mod.rs or tool.rs files to register or export the tool
- Optionally update types in crates/codegen/xai-grok-tools/src/types/ or registry/types.rs
- Add or update tests for the tool
- Update documentation if needed

## Notes

- Treat this as a scaffold, not a hard-coded script.
- Update the command if the workflow evolves materially.