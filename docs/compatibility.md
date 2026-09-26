# Compatibility and manual verification

This is an evidence ledger, not a support promise. The observations below were
recorded on 2026-09-06 or 2026-09-07 and were **not reverified on 2026-09-26**.
An old successful observation establishes only that the named combination
worked for the measured path at that time.

## Observed combinations

| Component | Observed environment | What was exercised | Recorded outcome | Current status (2026-09-26) |
| --- | --- | --- | --- | --- |
| macOS | Apple silicon, macOS 26.5 | Native build/test tooling and platform-specific process, Apple-event, plist, and launchd boundaries | The design and implementation checks ran on the reference machine. | Not currently reverified; Intel macOS and other macOS releases are unknown. |
| Ghostty | 1.3.1 | Read a live window containing 14 tabs; inspect tab/surface identity, working directory, title, and selection; create a tab in the front window | Collection fields were available. Creating a tab required an explicit target window. A scratch restore launched the expected process in the expected directory. | Not currently reverified. A fully closed Ghostty launch and the displayed resumed conversation remain unverified. Linux Ghostty is outside the implemented terminal adapter. |
| Claude Code | 2.1.263 | Registry lifecycle; `SessionStart` on startup and `/clear`; compact hook after sufficient context; transcript/title relation; scratch settings-based hook | Live registry identity and session-id behavior were measured. The scratch hook loaded and later captured a rotated id; one startup capture timed out. | Not currently reverified. Permanent hook installation was not activated. Authentication, trust prompts, and future registry/hook schema changes are unknown. |
| opencode | 1.18.29 (implementation verification); 1.18.15 also observed before an automatic update | Two concurrent TUIs in one directory; SQLite/WAL discovery; log/run timing; capture; resume into a new Ghostty surface | Capture represented uncertain joins as probable or ambiguous. Scratch restore launched with the expected session argument and directory. | Not currently reverified. Displayed-conversation restoration was not inspected, automatic updates are a known drift risk, and newer storage/log schemas are unknown. |
| Codex CLI | Bundled 0.153.4 (implementation verification); 0.147.0 observed during an earlier storage check | SQLite index plus transcript validation; first message; `/new`; retained rollout handles; capture and fresh resume candidates | CLI rows were distinguished from GUI, exec, and subagent rows. Retained handles were reported as ambiguous instead of being guessed exact. | Not currently reverified. Displayed-conversation restoration, hooks, and newer state/transcript schemas are unknown; Codex hooks were not exercised. |

## Manual release matrix

Before claiming compatibility with a new macOS, Ghostty, or agent release,
record the exact versions and date, then exercise these dimensions without
using personal production sessions:

| Dimension | Minimum evidence | Result on 2026-09-26 |
| --- | --- | --- |
| Build and static checks | Pinned toolchain builds; formatting, strict Clippy, tests, complexity, packaging, coverage, and dependency audit pass | Not recorded here; run the repository checks for the candidate revision. |
| Read-only capture | Each adapter detects its required schema, reads active scratch sessions, preserves argv boundaries, and does not create or mutate agent stores | Historical fixture/live evidence only; not currently reverified. |
| Identity and uncertainty | Claude exact identity is verified against process start time; same-directory opencode and post-`/new` Codex cases remain explicitly uncertain | Historical live evidence only; not currently reverified. |
| Restore plan | Missing directories/conversations are rejected; shell quoting survives spaces and apostrophes; ambiguous entries require explicit selection | Covered historically by tests and scratch checks; not currently reverified against current agent releases. |
| Ghostty submission | Existing-window and no-window paths create a new surface, submit once, and launch the expected resume command in the expected directory | Existing-window process/cwd path observed historically. No-window and displayed-conversation outcomes are unknown. |
| Agent readiness | Resumed TUI shows the intended conversation after any folder trust, authentication, migration, or update prompt | Unknown for all agents; command submission alone does not establish this. |
| Automation lifecycle | Dry-run matches the reviewed plan; install, immediate/interval capture, reinstall, failure rollback, and uninstall affect only owned files and services | Scratch launch-agent/hook evidence only. Permanent installation was not activated and is not currently reverified. |
| Failure behavior | Busy locks, schema drift, read-only WAL access, stalled subprocesses, partial adapter failure, and timeout cleanup preserve the previous snapshot | Covered historically by tests and fault checks; not currently reverified against current external versions. |

Keep evidence scoped: fixtures establish parsing and invariants; subprocess
tests establish local behavior; a launched resume command establishes process
arguments and directory; only inspecting the resumed TUI establishes that the
agent displayed the intended conversation.
