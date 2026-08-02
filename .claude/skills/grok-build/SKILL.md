```markdown
# grok-build Development Patterns

> Auto-generated skill from repository analysis

## Overview

This skill teaches you how to contribute effectively to the `grok-build` Rust codebase. You'll learn the project's coding conventions, how to add or update tools, commands, session logic, background tasks, settings, documentation, hooks, and authentication providers. Each workflow is documented with step-by-step instructions and example commands to streamline your development process.

## Coding Conventions

- **Language:** Rust
- **Framework:** Rust ecosystem (no external web frameworks)
- **File Naming:** Use `snake_case` for all file and directory names.
  - Example: `session_lifecycle.rs`, `task_scheduler.rs`
- **Import Style:** Prefer relative imports.
  - Example:
    ```rust
    mod session_lifecycle;
    use super::session::Session;
    ```
- **Export Style:** Use named exports (Rust's `pub`).
  - Example:
    ```rust
    pub struct SessionManager { ... }
    pub fn start_session() { ... }
    ```
- **Commit Messages:** Freeform, usually short (~21 characters), no strict prefix.

## Workflows

### Add or Update Tool Implementation
**Trigger:** When you want to add a new tool or update an existing tool's logic in the grok-build ecosystem.  
**Command:** `/add-tool`

1. Create or update files in `crates/codegen/xai-grok-tools/src/implementations/grok_build/<tool_name>/`.
2. Update `mod.rs` or `tool.rs` to register or export the tool.
    ```rust
    // In mod.rs
    pub mod my_new_tool;
    ```
3. Optionally update types in `crates/codegen/xai-grok-tools/src/types/` or `registry/types.rs`.
4. Add or update tests for the tool.
5. Update documentation if needed.

---

### Add or Update Slash Command
**Trigger:** When you want to add a new user-facing slash command or modify an existing one.  
**Command:** `/add-slash-command`

1. Create or update a file in `crates/codegen/xai-grok-pager/src/slash/commands/<command>.rs`.
2. Update `mod.rs` or `registry.rs` to register the command.
    ```rust
    // In mod.rs
    pub mod my_command;
    ```
3. Update user documentation in:
    - `crates/codegen/xai-grok-pager/docs/user-guide/04-slash-commands.md`
    - or `tutorial/05-slash-commands.md`
4. Optionally add or update tests.

---

### Add or Update Session Lifecycle or Management
**Trigger:** When you want to change how sessions are created, ended, persisted, or cleaned up.  
**Command:** `/session-lifecycle`

1. Edit session lifecycle logic in `crates/codegen/xai-grok-shell/src/agent/mvp_agent/session_lifecycle.rs` or related session files.
2. Update session management code in `crates/codegen/xai-grok-shell/src/session/*`.
3. Update or add tests in `crates/codegen/xai-grok-shell/src/session/acp_session_tests/*` or `tests/test_session_*`.
4. Update TUI or dashboard session views if needed.

---

### Add or Update Background Task or Scheduler
**Trigger:** When you want to add new background task types, update scheduling logic, or fix task lifecycle bugs.  
**Command:** `/add-scheduler-task`

1. Edit or create files in `crates/codegen/xai-grok-tools/src/implementations/grok_build/task/*` or `scheduler/*`.
2. Update related types in `task/types.rs` or `scheduler/types.rs`.
3. Update UI in `crates/codegen/xai-grok-pager/src/views/tasks_pane.rs` or background task views.
4. Add or update tests for task/scheduler logic.

---

### Add or Update Settings or Configuration
**Trigger:** When you want to add a new setting, change configuration behavior, or update the settings UI.  
**Command:** `/add-setting`

1. Edit or add setting definitions in `crates/codegen/xai-grok-pager/src/settings/defs.rs` or `registry.rs`.
2. Update settings UI in `settings_modal/*` or related `app/dispatch/settings/*` files.
3. Update documentation in `docs/user-guide/05-configuration.md`.
4. Add or update tests for settings.

---

### Add or Update Documentation or Tutorial
**Trigger:** When you want to document new features, update guides, or add onboarding tutorials.  
**Command:** `/add-docs`

1. Edit or add markdown files in `crates/codegen/xai-grok-pager/docs/user-guide/*` or `docs/tutorial/*`.
2. Update `README.md` or related documentation indices.
3. Optionally update code comments or inline docs.

---

### Add or Update Hook or Plugin
**Trigger:** When you want to add a new hook/plugin or update hook/plugin discovery, config, or runner logic.  
**Command:** `/add-hook`

1. Edit or add files in `crates/codegen/xai-grok-hooks/src/*`.
2. Update or add example hooks in `crates/codegen/xai-grok-hooks/examples/*`.
3. Update documentation in `docs/custom-hooks.md` or `user-guide/10-hooks.md`.
4. Add or update integration tests.

---

### Add or Update Auth Provider or Authentication Flow
**Trigger:** When you want to add a new auth provider, update OIDC/device code flows, or fix auth bugs.  
**Command:** `/add-auth-provider`

1. Edit or add files in `crates/codegen/xai-grok-shell/src/auth/*`.
2. Update related config or provider registration.
3. Update or add tests in `auth_provider_tests.rs` or `test_auth_provider_e2e.rs`.
4. Update documentation if needed.

---

## Testing Patterns

- **Framework:** Unknown (Rust-based, but test files may use custom or standard Rust test harnesses)
- **File Pattern:** Some tests are in `.test.ts` files, but most Rust tests are likely in `*_tests.rs` or inline `#[cfg(test)]` modules.
- **Example:**
    ```rust
    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_session_creation() {
            // test logic here
        }
    }
    ```

## Commands

| Command               | Purpose                                                         |
|-----------------------|-----------------------------------------------------------------|
| /add-tool             | Add or update a tool implementation in grok-build               |
| /add-slash-command    | Add or update a slash command in the TUI                        |
| /session-lifecycle    | Modify session lifecycle or management logic                    |
| /add-scheduler-task   | Add or update background task or scheduler logic                |
| /add-setting          | Add or update user/system settings or configuration             |
| /add-docs             | Add or update documentation or tutorials                        |
| /add-hook             | Add or update a custom hook or plugin                           |
| /add-auth-provider    | Add or update an authentication provider or authentication flow |
```
