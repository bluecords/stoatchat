# Environment facts — stop re-deriving these

*Canonical copy. Symlinked/copied into each repo's `.claude/rules/`. Edit here.*

These are the specifics that produce the visible `failed … failed … failed` runs. Every
one below has actually cost a retry.

## This machine

- **The `Bash` tool is Git Bash (POSIX sh), not PowerShell and not cmd.** Use `/c/Users/...`,
  `$VAR`, forward slashes. The `PowerShell` tool is separate and takes PowerShell syntax.
- **NEVER run `find /` (or `grep -r /`) on this machine.** Git Bash's `/` is the whole Windows
  install — it walks `C:\Windows`, every `node_modules`, `AppData`, OneDrive placeholders and any
  mapped drive. **Measured 2026-09-06: a `find /` ran for 55 minutes** after the task that started
  it had already finished and reported, and Bunjie is the one who spotted it. **Scope every search
  to a real project root** (`/c/Users/bunjm/<repo>`), or use `Glob`/`Grep`, which are indexed.
  ⚠️ **This applies doubly to SUBAGENTS.** A spawned agent does **not** inherit these rules — it
  starts cold with no `.claude/rules/` context. **Put the search-scope constraint in the prompt
  itself**, and prefer naming the exact directories it should look in. The 55-minute run above was
  a subagent's, and it kept running as an orphan after the agent returned.
- **`gh` is at `/c/Program Files/GitHub CLI/gh.exe`** — quote the whole path. Bare `gh` may not resolve.
- **`gh api` endpoints must NOT start with `/`.** Git Bash rewrites a leading-slash argument into
  a Windows filesystem path, and the error reads like a bad endpoint:
  `invalid API endpoint: "C:/Program Files/Git/orgs/..."`. Write `gh api orgs/bluecords/...`.
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

## `gh` and forks — this cost a wasted PR attempt

- **In a forked repo, `gh` resolves to the UPSTREAM, not our fork.** In `stoatchat` (origin
  `bluecords/stoatchat`, upstream `stoatchat/stoatchat`), `gh pr create` targeted upstream and
  failed with *"No commits between main and <branch>"* plus *"Head sha can't be blank"* — which
  reads like a push problem and is not one. **Always pass `--repo bluecords/<repo>`** for
  `pr create`, `pr merge`, `pr checks` and `run list`. Check with `gh repo view --json nameWithOwner`.

## Driving Bunjie's handset (added 2026-09-03)

- `adb` is at `/c/Users/bunjm/AppData/Local/Android/Sdk/platform-tools/adb`, not on PATH.
- ⚠️ **`adb shell input tap` is NOT a faithful finger** — a bare tap registered as hover only, and
  `input swipe x y x y 120` did not reproduce behaviour he sees. **Do not conclude anything from it.**
- ✅ **Use CDP.** `adb forward tcp:9445 localabstract:chrome_devtools_remote_<chromePID>`
  (PID from `adb shell cat /proc/net/unix | grep devtools`). **The socket only answers once Chrome
  is foregrounded with a live tab** — it times out before that, which looks like a broken setup and
  is not. Pick the target whose `document.hidden` is false.
- `Input.dispatchTouchEvent` produces touches Chrome turns into real synthesised clicks, which is
  the whole point. A MutationObserver plus `window.__ev` capture arrays beat screenshots for
  anything that opens and closes quickly.

## Secrets: the redaction rule that actually works (2026-09-03, learned twice in one session)

- **Never print a line that may contain a value.** Two secrets reached a transcript in one session:
  one by `sed -n '1,40p'` on `nac-server/Revolt.toml` (its FIRST non-comment line is a secret), and
  one by a redaction `sed "s/=.*/=<redacted>/"` that silently did nothing because the file was YAML
  (`KEY: value`, not `KEY=value`). **That second one is the class-3 trap already in `FEEDBACK.md`,
  committed while cleaning up the first.**
- **Use `grep -c` to test presence, and sha256 fingerprints to compare.** Comparing two secrets by
  `sha256sum | cut -c1-12` proves match/mismatch without either value being shown, and is how the
  sponsor rotation was verified end to end.
- **Move a secret as a FILE**, never through a command line: generate on the box, `docker cp` it in,
  read it from disk. Anything interpolated into a command is in the transcript.
- **`nac-server/Revolt.toml` specifically: do not dump it.** Grep the exact key.

## `gh run list --limit 1` right after a merge returns the PREVIOUS run

Hit twice on 2026-09-03. The new workflow run is not registered yet, so you watch an already-finished
run, see success, and conclude the deploy landed when it has not started. **Confirm the run's
`displayTitle` matches the commit you just merged**, or probe the destination (served bundle hash,
live config) rather than trusting the watch. `gh run watch` also returned exit 0 once while the run
was still `in_progress`.

## `docker cp` of a SQLite database gives you a STALE snapshot (2026-09-03)

**n8n's `database.sqlite` is in WAL mode.** Copying only that file out leaves every
transaction still in `database.sqlite-wal` behind, so you read the last checkpoint, not
the present.

**This cost a self-inflicted production outage.** Reading n8n that way showed the sponsor
poller "dead for an hour" and an execution "stuck in new". Neither was true - with the WAL
included, every poll had succeeded on schedule the whole time. Acting on the phantom, the
DB was copied out, edited, and copied BACK - which replaced the live file with a stale
snapshot and left it **root-owned**, so n8n crash-looped on `SQLITE_READONLY` for ~10
minutes. Integrity survived, by luck rather than design.

- **Copy all three files together** (`database.sqlite`, `-wal`, `-shm`) or you are reading history.
- **Never `docker cp` a database back in.** It clobbers newer WAL-committed writes and
  resets ownership. `docker cp` writes as root; the container runs as `node` = **uid 1000**,
  which on this host is `ubuntu`. `chown node:node` on the HOST fails silently - there is no
  `node` user there. Use `chown 1000:1000`.
- **`docker exec` cannot run against a STOPPED container.** A `chown` step placed between
  `docker stop` and `docker start`, guarded with `|| true`, silently does nothing - which is
  exactly how the readonly file shipped.
