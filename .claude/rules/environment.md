# Environment facts — stop re-deriving these

*Canonical copy. Symlinked/copied into each repo's `.claude/rules/`. Edit here.*

These are the specifics that produce the visible `failed … failed … failed` runs. Every
one below has actually cost a retry.

## This machine

- **The `Bash` tool is Git Bash (POSIX sh), not PowerShell and not cmd.** Use `/c/Users/...`,
  `$VAR`, forward slashes. The `PowerShell` tool is separate and takes PowerShell syntax.
- **`gh` is at `/c/Program Files/GitHub CLI/gh.exe`** — quote the whole path. Bare `gh` may not resolve.
- **Foreground `sleep` is blocked by the harness.** To wait on something, use `run_in_background`
  and let the completion notification arrive. Do not chain short sleeps to get around it.
- **`pyyaml` is not installed.** `python -m pip install --quiet pyyaml` first if you need to
  validate YAML — and validate YAML you edited, always.
- Scratch files go to the session scratchpad, generated deliverables go to
  `C:\Users\bunjm\Downloads\`. Never the Desktop (OneDrive-redirected).

## Line endings — this bites anything hashed or compared

- **Working trees are CRLF. Repo blobs are LF. Linux serves the LF bytes.**
- Measured 2026-08-30: the same file hashed `cf7aba3a…` on disk and `e88b797c…` as the blob.
  **The wrong one was about to be published as a consent-record hash.**
- Any Python string-replace against a file must build its patterns with the file's own newline:
  `nl = '\r\n' if '\r\n' in s else '\n'`. A pattern joined with `\n` silently matches nothing.
- `.gitattributes` pins the files where this matters. If you add a file whose bytes are
  compared or hashed, pin it too.

## Editing source — the single biggest source of wasted turns

- **Use the `Edit` tool, not an inline `python - <<'PY'` heredoc.** `Edit` fails loudly when the
  anchor is missing or ambiguous; a heredoc silently matches nothing or matches twice.
- **Heredocs with nested quotes break the outer shell.** Two failures on 2026-08-30
  (`unexpected EOF while looking for matching '`). If a script is genuinely needed, `Write` it
  to a file and run the file.
- If you do use a string-replace, **assert the anchor is unique** before writing. An anchor that
  matched twice cost a retry on `Client.ts` the same night.

## Verifying things — the failures here are the expensive ones

- **Piping to `grep` throws away the command's exit code.** Run it, check `$?`, then filter.
- **`git stash` on a clean tree stashes nothing**, so "stash, measure, pop" is not a baseline.
  Diff against `origin/main` instead — e.g. `git show origin/main:path > /tmp/base`.
- **A count is only a baseline if you measured it the same way twice.** State the before number.
- **Before merging or deploying: probe the DEPLOYED system**, not the diff. `curl` the live
  endpoint, read the pinned image tag. See `FEEDBACK.md` class 1.

## Browser / preview tools

- `navigate` sometimes returns "denied or failed" on the first call after the pane opens.
  Call it once more before treating it as a real failure.
- **`navigate` drops the query string from `location.search` after load.** A `?flag` harness
  reads correctly at load and then appears empty — do not chase it. Force the branch in code
  instead, verify, then revert.
- Screenshots can time out while the page is still painting. Retry once; check
  `curl -o /dev/null -w "%{http_code}"` against the port to confirm the server is actually up.
- `preview_start` uses `.claude/launch.json`. A `cwd` key is **not** honoured — put the working
  directory in the command itself.
