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
  automatically. **Claude does the bump** of `nac-server/compose.yml` and all the work (Bunjie never bumps), but **while he is present he approves the prod deploy; a handed-over block of time ("going to work", "going to bed") is the approval** (`[RULED BY BUNJIE]` 2026-10-01). Verify the live API reports the new version, then tell him what to validate.
- **Clients compute permissions THEMSELVES.** `revolt-permissions` is server-side; web has its
  own `calculator.ts` and Android its own `Permissions.kt`, and neither knows about consent
  state. **Anything enforced only inside `calculate_server_permissions` is invisible in every
  UI** — the member sees a normal-looking app that fails at every action.
