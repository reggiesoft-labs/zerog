# Agent Guidelines

## Repository Scope

- This is a monorepo. Use the repository root as the working directory. Understand the full end-to-end system contract before scoping changes to the minimal necessary files.
- `src/` and `tests/` contain the Rust backend.
- `gui/src/` contains the frontend, and `gui/src-tauri/` contains its Tauri bridge.
- For frontend tasks, search `gui/src`, `gui/src-tauri/src`, and relevant GUI tests before searching the Rust backend.
- For backend tasks, search `src` and `tests` before searching the GUI.
- For cross-boundary tasks, inspect the relevant backend, bridge, and frontend paths explicitly.
- When modifying shared platform traits, signatures, or types (`src/platform/shared/`), audit and update all platform targets (`src/platform/windows/`, `src/platform/macos/`, `src/platform/linux/`).
- Never recursively inspect `target/`, `node_modules/`, or generated Tauri schemas.
- Read `Cargo.lock` or `gui/package-lock.json` only for dependency-related tasks.
- Prefer `rg` with explicit subsystem paths over broad repository scans.
- Never run filesystem-wide scans (`find /`, `find ~`, `ls -R /`, `grep -r /`). Resolve tooling files under known paths only (e.g. `gui/node_modules/@tauri-apps/cli/`, `~/.nvm/...`).
- Maintain lifecycle script parity (`dev`, `build`, `test`) between root `package.json` and `gui/package.json`.

- Run tests as sparingly as possible when working on code in this project.
- Always run tests quietly (`cargo test -q` or `cargo test --lib -q`) to minimize output noise and token consumption.
- Prefer library unit tests (`cargo test --lib -q`) during iteration to avoid compiling and executing separate integration test binaries.
- Prefer targeted test execution with exact names (e.g., `cargo test <test_name> -- --exact`) over running full suites.
- Use `cargo check --quiet --all-targets` or `cargo clippy --quiet` for fast syntax and type validation before running tests or committing backend changes.
- For GUI/frontend tests in `gui/`, run Vitest in single-run mode for targeted files (e.g., `npm test -- --run <file>`).
- Do not run tests before making changes, only after.
- Do not run tests when asked to commit and push unless explicitly requested.
- This project-specific rule overrides broader Git defaults: commit and push to `origin/main` after every change for this project.

## State Machine & Async Lifecycle Protocol

- **State & Timer Transition Matrix**: Before modifying any asynchronous loop, event handler, timer, or state variable in multi-mode runtime components (e.g., `libei_capture`, `input_loop`, `edge_transitions`, `packet_handler`), explicitly trace the transition matrix: `(Current Mode, Event / Timer Expiry) -> Action / Next Mode`.
- **Delayed Timer Auditing**: For any timer, deadline, or delayed callback (e.g., `pending_release`, cooldown timers), audit behavior across all modes (`DeliverLocally`, `SuppressLocalDelivery`, disconnected). Never assume a delayed callback only executes in the mode where it was armed.
- **Mode-Boundary Test Requirement**: "Run tests as sparingly as possible" does NOT exempt verifying state and timer transitions. Any stateful or timer change must have test coverage asserting behavior when delayed timers fire across mode boundaries (e.g., timer expiring after entering remote control).


