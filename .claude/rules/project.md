# stoatchat specifics

- **Check per-crate, not the workspace.** A workspace `cargo check` fails building
  `openssl-sys` on Windows (missing system library) — that is the machine, not your change.
  Use `cargo check -p revolt-models -p revolt-database --features revolt-database/mongodb`,
  and `-p revolt-delta`, `-p revolt-permissions`.
- **`routes::channels::message_pin::test::pin_message` is a KNOWN FLAKE.** It panics at
  `crates/delta/src/util/test.rs:241` with `internal error: entered unreachable code`. That
  `unreachable!()` ends `wait_for_event`, whose own comment says it has no timeout. Confirmed
  2026-08-31: the identical tree passed on `main` and on the feature branch, and failed only on
  the release branch. **Re-run before investigating.**
- **The release chain is Claude's**, per `claude-repo/FEEDBACK.md` → AUTONOMY. Merge the feature
  PR, then merge the `chore(main): release X.Y.Z` PR release-please opens; images build
  automatically. **Bunjie's gate is the PROD deploy only** — bumping `nac-server/compose.yml`.
- **Clients compute permissions THEMSELVES.** `revolt-permissions` is server-side; web has its
  own `calculator.ts` and Android its own `Permissions.kt`, and neither knows about consent
  state. **Anything enforced only inside `calculate_server_permissions` is invisible in every
  UI** — the member sees a normal-looking app that fails at every action.
