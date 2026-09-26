# 1. Capture runs as short-lived invocations, not a resident process

**Status:** accepted by the maintainer, 2026-09-06.

## Context

Capture has to answer, at some moment while sessions are alive: which agent
sessions exist, in which directory, under which id, and in which terminal tab.
Everything measured on 2026-09-06 on the reference machine bears on how often
and how cheaply that moment can be produced.

**What capture reads, and what it costs.**

| Source | Read | Cost |
|---|---|---|
| Claude Code registry | one small JSON file per live session | milliseconds |
| opencode | one read-only query against a 152 MB SQLite database | milliseconds |
| Codex | first line of each recent transcript, newest first | milliseconds |
| Process table | one listing with pid, parent, tty, start time | milliseconds |
| Ghostty | one scripting-bridge call listing tabs with id, working directory, title | 0.54 s as a per-tab loop; 0.09 s fetching whole collections |

A hand-rolled full capture across all four, including interpreter startups,
took 0.6 s without the terminal and about 1.1 s with it. Rewriting the
terminal query to fetch whole collections instead of looping per tab brought
it to under 0.1 s, and compiled candidates spend single-digit milliseconds on
the agent reads. A full capture is therefore about 100 ms. Capture is cheap
enough to run every minute and on every agent start without anyone noticing.

**What changes between captures, and how fast.**

- Claude Code rotates the session id on clear, not on compaction. A polled
  snapshot taken before a clear points at the pre-clear transcript, which still
  resumes, just to the older conversation. The session-start hook fires on
  startup, clear, and compaction with the current id and the transcript path,
  within the same quarter-second the registry entry appears. Event-driven
  capture for this agent is therefore exact and nearly free.
- opencode creates no session row until the first message is sent, so a
  capture at launch sees a process with no session. The only way to pair a
  process with its session when two share a directory is the agent's log,
  which records a per-process run id against each created session and whose
  first line lands about a second after process start. That pairing needs the
  current log file, which rotates. Capture soon after launch is worth more
  than capture often.
- Codex transcripts carry their own directory and origin. They need no
  capture at all for identity, only for "was open" and tab placement.
- Ghostty exposes tab id, working directory, and title, but no tty or pid, and
  nothing about the tab reaches the child process environment. The tab is
  matched to the session by working directory plus title. Claude Code writes
  the title it sets into the transcript as an ai-title record, so for that
  agent the match is exact whenever titles differ. Titles change during a
  session, so the pairing is only as fresh as the last capture.

**What must never happen.** Capture must not write into an agent's directory,
signal or attach to a process, or overwrite a good snapshot with an empty one.
Restore must run with no agent alive, from a cold shell, so the tool must
exist outside any agent plugin.

**Where it runs.** The reference machine had 957 processes and 90 % swap use
during the investigation, and the user's original complaint was process
accumulation. Adding a permanently resident process to that machine is not
neutral.

## Options

**A. Resident background process.** Launched at login and kept alive. Polls on
an interval and could watch the registry directory for changes.

- For: lowest latency; incremental state between runs; one place to hold a
  file watcher.
- Against: one more always-on process on a machine already suffering from
  them; if it dies, capture stops silently and the user finds out at restore
  time; requires a supervisor definition, restart policy, and log rotation of
  its own; state held in memory is lost on the crash it exists to survive,
  so it would write to disk on every change anyway, at which point it is a
  poller with a longer lifetime.

**B. Short-lived periodic invocation, plus hooks.** The launch system starts
the tool on an interval; it captures, writes the snapshot if anything changed,
and exits. The Claude Code session-start hook runs the same command, giving
exact, immediate capture for that agent.

- For: nothing resident; every run starts from disk, so there is no state to
  lose; the same binary serves capture, hook, and restore, which keeps the
  restore-runs-standalone rule trivially true; failure is visible as a missing
  or stale snapshot file rather than a dead process.
- Against: a window of up to one interval in which a launch, clear, or title
  change is not yet recorded. At a one-minute interval and 1.1 s per run this
  is under 2 % duty and a worst case of one stale title or one missing
  just-opened opencode session. The hook closes the window entirely for Claude
  Code, which is the agent with the most sessions and the only one that
  rotates ids.

**C. Hooks only.** No scheduler; rely on agent hooks.

- Against: only Claude Code has a verified hook; sessions that predate the
  hook's installation are never captured; nothing observes opencode or tab
  titles. Rejected as a sole mechanism, kept as a component of B.

## Decision

Option B. Capture is a subcommand that runs to completion. It is invoked two
ways: by the platform launch system on a one-minute interval, and by the Claude
Code session-start hook on startup, clear, and compaction. Both paths run the
same full capture; a hook-triggered run is not special-cased to one session,
because a full capture costs about a second and special-casing would create a
second code path to keep correct.

Rules the subcommand must satisfy, each traceable to a measurement above:

1. Exit successfully and quickly from the hook path, whatever happens. A
   session-start hook runs synchronously before the agent is usable, so the
   tool must never make an agent fail to start. Enforce an internal deadline;
   with collection-shaped terminal queries the full capture fits inside it,
   so the terminal query is skipped only if the deadline is actually at risk.
2. Write only when the captured set differs from the latest snapshot. This
   keeps history meaningful and disk writes rare.
3. Never replace a non-empty snapshot with an empty capture.
4. Record, per session, the confidence of the process-to-session and
   tab-to-session pairings separately, since they fail independently.
5. Detect the storage format of each agent on every run. opencode updated
   itself between two tests thirty minutes apart.

## Consequences

The tool is one executable with subcommands for capture, restore, and
install/uninstall of the scheduler entry and hook. This settles the packaging
shape, and it sets the constraints the language choice has to meet:

- **Cold start matters.** The binary runs roughly 1,500 times a day on the
  interval plus once per Claude Code start, clear, and compaction, and the hook
  path sits on the agent's startup critical path. Startup in the low tens of
  milliseconds is the target; hundreds is acceptable only if the hook path can
  skip the terminal query.
- **Read-only SQLite access** without an external client on the path.
- **Spawning a subprocess** for the terminal scripting bridge; there is no
  supported alternative to it on this platform.
- **Atomic file writes** for snapshots, with a bounded history.
- **A single self-contained artifact** that runs with no agent, runtime
  manager, or package registry available, because restore runs from a cold
  shell after a reboot.

The language and file formats were subsequently settled in decision 0002:
Rust and TOML. Distribution remains open.
