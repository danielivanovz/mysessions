# My Sessions

Restore terminal-resident coding agent sessions into terminal tabs after a
restart.

Agents like Claude Code, opencode and Codex persist their conversations to
disk, but the link between a conversation and the terminal tab it was running
in lives only in the running process. Reboot, and the tabs come back as bare
shells: the conversations are all still there, but which ones were open, and
where, is gone.

`mysessions` captures that mapping while the sessions are alive, and reopens them
afterwards.

> **Status: early implementation.** An interactive terminal browser, capture,
> Ghostty restore, and the macOS scheduler/Claude hook installer are implemented.
> Claude's registry supplies
> exact process identity. opencode log timing and Codex open transcripts supply
> candidates, labelled probable or ambiguous.
> Decisions in [`docs/decisions/`](docs/decisions/).

```sh
cargo build --release
./target/release/mysessions               # open the terminal browser
```

Use `mysessions` from a standalone terminal. The browser starts with the newest
snapshot and selects valid exact/probable entries. Ambiguous entries require
an explicit selection; missing conversations cannot be selected.

Claude rows use the conversation title from the transcript, falling back to
the registry name when no title is available. Both remain searchable. The
selected conversation's title also wraps in the details area below the table.

| Key | Action |
| --- | --- |
| Up/Down or j/k | Inspect a session or installation path |
| Space | Toggle a session |
| / | Search titles, projects, agents and session ids; Enter finishes typing |
| a / n | Add suggested matches / clear all selections |
| [ / ] | Older / newer snapshot |
| Enter | Review every selected session, then Enter again opens new tabs |
| c | Capture open sessions and load the latest saved snapshot |
| r | Refresh saved history and the current snapshot |
| i / u | Review installation / removal of automatic capture |
| Esc | Cancel a review or clear a search |
| q or Ctrl-C | Quit |

The restore review includes selections hidden by search. It validates each
conversation again before opening a tab, shows individual results, and prevents
another attempt for the same conversation during that browser run. Starting
the browser again can open duplicates.

Installation review lists affected files and the settings backup path. Use
Up/Down to inspect full paths, then Enter to apply or Esc to cancel. Enabling
automatic capture starts the minute timer immediately and adds the Claude
SessionStart hook. Opening the browser alone changes no files or services.
Capture uses the same bounded command as the CLI and can take up to ten seconds.
The browser needs at least 60 columns by 20 rows.

The explicit commands remain available for shell use and automation:

```sh
./target/release/mysessions browse --snapshot /path/to/snapshot.toml
./target/release/mysessions capture --dry-run   # print what would be snapshotted
./target/release/mysessions capture             # write a snapshot if the set changed
./target/release/mysessions restore             # print resume commands from the latest snapshot
./target/release/mysessions restore --apply     # open new Ghostty tabs
./target/release/mysessions restore --session SESSION_ID --apply
./target/release/mysessions restore --snapshot /path/to/snapshot.toml
./target/release/mysessions install --dry-run   # review the installation
./target/release/mysessions install             # copy binary and activate timer/hook
~/.local/bin/mysessions uninstall --dry-run     # review automation removal
~/.local/bin/mysessions uninstall               # keep binary, snapshots and backups
```

Snapshots live under `~/.local/state/mysessions/` (or `$XDG_STATE_HOME/mysessions`) and
are readable and paste-able without the tool.

Capture opens agent databases read-only and checks their required schema on
every run. If an adapter fails, the command reports the error and preserves
the previous snapshot. `--dry-run` still prints the partial result and exits
unsuccessfully, so it cannot be mistaken for a complete capture.

An opencode run can visit multiple conversations without logging UI switches.
Codex also retains old transcript handles after `/new`. These cases produce
ambiguous candidates, which may include more entries than there are open
agent tabs. No recency guess is presented as exact.

Restore validates directories and conversations on disk before opening tabs.
Missing conversations are skipped and reported. Probable selections are
included; ambiguous/unknown process selections are skipped unless explicitly
selected with `--session` or included with `--include-ambiguous`. Tab ambiguity
does not prevent restore, because restore always creates a new tab. It submits
an explicit, quoted `cd ... && resume-command` to that tab's shell. A submitted
command is not a guarantee that the agent passed its own trust/auth prompts.
Repeated restore commands can open duplicates; existing tab ids are not reused.

On macOS, `install` copies the running executable to `~/.local/bin/mysessions`,
writes `~/Library/LaunchAgents/local.mysessions.capture.plist` (`StartInterval = 60`,
`RunAtLoad = true`, no daemon), and adds one
[Claude SessionStart hook](https://code.claude.com/docs/en/hooks#sessionstart)
to `~/.claude/settings.json`. Existing hooks and other settings are preserved.
Changes to settings are backed up in the My Sessions state directory; installation
rolls back if launchd activation fails. Repeating install is idempotent.
Uninstall removes only the managed hook and launch agent. The installed binary
remains available for standalone restore, even if this checkout is removed.
Add `~/.local/bin` to your shell's PATH or invoke that absolute path directly.

When the running executable was installed by Homebrew, `install` copies
nothing. The launch agent and hook invoke Homebrew's
`$(brew --prefix)/opt/mysessions/bin/mysessions` link instead, which
`brew upgrade` repoints at each new release, so upgrades need no reinstall.
Switching an existing installation to Homebrew replaces the hook and leaves
the earlier `~/.local/bin` copy in place. Run `mysessions uninstall` before
`brew uninstall`; otherwise the timer keeps trying to start a removed
executable.

Capture and restore share a nonblocking lock, preventing hook/timer captures
from racing or replacing the selected snapshot during restore. Changes within
one second retain separate snapshot files. `capture --hook` always returns
success and has a 3-second deadline; normal capture has a 10-second deadline.
The supervisor stops only its own capture subprocesses on timeout. Busy starts
can miss a hook capture; the minute timer is the backstop. Diagnostics go to
`~/.local/state/mysessions/capture.log` (or the configured XDG state directory).
Set `MYSESSIONS_TRACE_CAPTURE=1` when diagnosing a stalled adapter.

## Development

Develop and run the native tests on macOS with Rustup, Xcode command line
tools, Make, and Python 3. The repository pins Rust 1.89.0, rustfmt and Clippy.

```sh
make check          # formatting, strict linting, tests, cyclomatic complexity
make fmt            # apply the formatter
make lint
make test
make coverage       # all targets/features; requires cargo-llvm-cov; 75% line floor
make package        # verify the publishable crate contents and build
make security       # fail on RustSec vulnerabilities and warnings; requires cargo-audit
make complexity     # detailed JSON: target/complexity/report.json
MYSESSIONS_TEST_SEED=42 QUICKCHECK_TESTS=1000 cargo test --locked property_
```

Install the pinned coverage tool with
`cargo install cargo-llvm-cov --version 0.8.7 --locked` and add Rust's
`llvm-tools-preview` component. Coverage runs separately from `make check` so
the normal local feedback loop does not repeat the full test suite.

Install the pinned dependency audit tool with
`cargo install cargo-audit --version 0.22.2 --locked`. `make security` audits
the locked dependency graph and fails on vulnerabilities or warnings. Advisory
ignores must be individually justified and documented; none are currently
configured.

Clippy's standard and pedantic lints run with warnings treated as errors,
including undocumented unsafe blocks. Explicit persisted identity names
(`session_id`, `surface_id`) are allowed. Keep unsafe code at the native OS
boundary, use checked process-id conversions, and preserve contextual errors.

The first complexity check builds Mozilla's
[rust-code-analysis](https://github.com/mozilla/rust-code-analysis) into
`target/tools`, using a pinned source revision and its dependency lockfile.
The published crate predates Rust's raw-pointer syntax. The gate requires
**cyclomatic complexity of 15 or less per function or closure**, including
tests, and fails on parser errors or missing reports. It checks known straight-line,
branching, fallible and nested-closure examples before measuring the source.
Nested functions are measured separately. This is a source metric: macros
are not expanded, and the number does not prove correctness. Clippy's
`complexity` lint group is separate from this numeric check.

[QuickCheck](https://github.com/BurntSushi/quickcheck) checks snapshot round
trips, shell argument preservation, hook preservation/idempotence, log and
process-argument parsing, terminal field preservation, safe display text, and exact tab pairing
under reordered inputs. Each property runs 256 cases with seed 0 by default,
shrinks failures, and rejects zero-case runs. Override the seed/count to
explore further and retain failures as explicit regression tests. Tests use
scratch stores and their own subprocesses; they never launch agents or
activate the installation.

The macOS GitHub Actions workflow runs the same checks and an additional
1,000 cases per property using the run number as a reproducible seed.

## What this is not

Your transcripts are already safe — every agent examined writes them to disk
continuously, and they survive reboots without help. This tool does not back
them up and does not prevent data loss.

It exists because the *mapping* degrades, and because recovering it by hand
across a dozen tabs is enough friction that people abandon sessions instead.

In fact you can often recover without any tooling at all. Listing transcripts
newest-first puts the ones you had open at the top:

```sh
ls -t ~/.claude/projects/*/*.jsonl | head -20
```

On the reference machine that recovered all 12 open sessions out of 75 on
disk. The tool earns its place where that heuristic stops working: two sessions
in the same directory, a long history relative to open sessions, idle sessions
that sort low, and knowing which tab held what — which recency cannot tell you
at any scale.

## Design

- [`docs/compatibility.md`](docs/compatibility.md) — dated, evidence-bound
  compatibility observations and the manual release matrix
- [`docs/decisions/`](docs/decisions/) — capture mechanism (short-lived runs
  plus hooks, no daemon) and implementation technology (Rust), each with the
  measurements behind it

## Scope

Agents whose session identity is held by a terminal process, restored into
terminal tabs on the same machine.

GUI agents are out of scope by construction — a desktop app reopens its own
conversations, and there is no tab to restore. Where an agent records the
origin of a session, that field is the filter.
