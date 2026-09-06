# roost

Restore terminal-resident coding agent sessions into terminal tabs after a
restart.

Agents like Claude Code, opencode and Codex persist their conversations to
disk, but the link between a conversation and the terminal tab it was running
in lives only in the running process. Reboot, and the tabs come back as bare
shells: the conversations are all still there, but which ones were open, and
where, is gone.

`roost` captures that mapping while the sessions are alive, and reopens them
afterwards.

> **Status: design stage.** No implementation yet, and the implementation
> technology is deliberately undecided. See [`docs/design.md`](docs/design.md).

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

- [`docs/design.md`](docs/design.md) — problem, findings per agent, adapter
  architecture, failure modes, open questions, and a verification log that
  separates what is proven from what is assumed
- [`docs/article-notes.md`](docs/article-notes.md) — raw material for a
  write-up: measurements, timeline, and the corrections made along the way

## Scope

Agents whose session identity is held by a terminal process, restored into
terminal tabs on the same machine.

GUI agents are out of scope by construction — a desktop app reopens its own
conversations, and there is no tab to restore. Where an agent records the
origin of a session, that field is the filter.
