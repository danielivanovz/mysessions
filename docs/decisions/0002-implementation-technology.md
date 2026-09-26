# 2. Implementation technology

**Status:** accepted, 2026-09-06 — Rust. The measurements and trade-offs
below are the basis; the recommendation section was adopted as written.

Settled at the same time because they follow from the language and from
requirements already in the design:

- **Snapshot format: TOML.** The design requires the snapshot to stay a
  human-readable, paste-able recovery list with the recency fallback as
  prose even if the tool is broken. TOML carries comments, so each entry
  can sit beside its own resume command and the header can explain
  recovery. JSON cannot hold comments and would need a sidecar.
- **Configuration format: TOML.** Same parser, same mental model.
- **Command name: `mysessions`**, matching the product name My Sessions. One
  constant; trivial to change until a remote exists.

## Constraints being tested

From the capture-mechanism decision: fast cold start because the binary runs
on every agent start and on a one-minute timer; embedded read-only SQLite for
two of the three agents; a subprocess or in-process call to the terminal's
scripting bridge; atomic snapshot writes; and one self-contained artifact that
runs from a cold shell with no runtime manager present.

## Measurements

All on the reference machine (Apple silicon, macOS 26.5), 30 runs each after
warm-up, median and 95th percentile wall time including process spawn. The
SQLite program opens the 152 MB opencode database read-only and runs the
session query. "Artifact" is the size of what would be shipped.

| Candidate | hello, median | hello, p95 | sqlite, median | sqlite, p95 | Artifact |
|---|---|---|---|---|---|
| Rust 1.89, rusqlite bundled | 2.3 ms | 2.7 ms | 3.3 ms | 3.7 ms | 2.0 MB |
| Swift 6.2, system SQLite | 2.6 ms | 3.0 ms | 5.2 ms | 5.7 ms | 0.1 MB |
| Go 1.25, modernc pure-Go SQLite | 2.7 ms | 3.0 ms | 6.8 ms | 7.1 ms | 9.7 MB |
| Bun 1.3 compiled executable | 8.8 ms | 9.3 ms | 11.5 ms | 12.2 ms | 63.1 MB |
| Bun 1.3 script | 9.5 ms | 10.2 ms | 12.0 ms | 12.6 ms | needs Bun |
| Python 3.13 | 17.7 ms | 18.6 ms | 20.4 ms | 21.1 ms | needs Python |
| Node 24, node:sqlite | 25.5 ms | 26.9 ms | 27.3 ms | 28.1 ms | needs Node |

Every candidate clears the cold-start constraint. The slowest is under 30 ms.
Cold start does not decide this.

The terminal query is the dominant cost and is independent of language:

| Terminal listing, 14 tabs | Median |
|---|---|
| `osascript` process alone, trivial script | 32 ms |
| Per-tab loop fetching three properties, via `osascript` | 537 ms |
| Same loop, in-process from a Swift binary | 536 ms |
| Whole-collection fetch, three events total, via `osascript` | 92 ms |
| Whole-collection fetch over every surface, including splits | 92 ms |

The cost is Apple-event round trips, not the subprocess. An in-process call
saves nothing. Asking for whole collections saves 445 ms and also covers split
panes. This is a scripting-shape decision, already made: fetch collections.

With that, a full capture across three agents and the terminal is roughly
100 ms in any compiled candidate. The hook path can include the terminal
query rather than skipping it.

## What actually discriminates

**Self-contained artifact.** Node and Python need a runtime on the path at
restore time, after a reboot, from a cold shell; on this machine both live
under a version manager whose shims are not on a bare shell's path. They fail
the constraint unless bundled, and bundling them is more work than the other
options. Bun's compiled executable passes but is 63 MB. Rust, Swift, and Go
pass natively.

**Portability.** Swift builds for Linux but its ecosystem there is thin, and
the terminal adapter is platform-specific anyway: Ghostty on Linux has no
scripting bridge, so a Linux port needs a different terminal adapter
regardless of language. Agent adapters are the shared part, and those are
file and SQLite reads that every candidate does the same way. Go and Rust
cross-compile without ceremony; Swift and the in-process bridge buy nothing
measurable.

**SQLite dependency shape.** Rust with the bundled feature and Go with the
pure-Go driver carry their own SQLite, so behaviour does not vary with the
system library. Swift and Python use the system library, which macOS ships.
Bun and Node ship their own. None of this is a blocker; the bundled forms
are the most predictable.

**Strictness against undocumented formats.** The largest ongoing risk is
storage drift, and the agreed rule is detect the format and fail hard rather
than degrade quietly. Typed deserialisation with exhaustive matching makes
"a field I did not expect" a compile-time or load-time error. Rust's
serde-based approach is the strictest by default; Go is strict enough with
explicit checks; dynamic languages need discipline to get the same effect.

**The opencode shim.** opencode plugins run inside its own Bun runtime and
are written in JavaScript or TypeScript, whatever the tool's language. The
shim is a few lines that spawn the binary on session events. It does not
argue for a JavaScript tool; it is a separate small file in every case.

**Distribution.** All three compiled candidates fit a Homebrew formula or a
release binary; Rust adds `cargo install` and Go adds `go install` for people
who have those toolchains. Bun would ship through npm as a large binary.

## Recommendation, for discussion

Rust, with Go as the close alternative. Reasoning in order of weight:

1. Smallest self-contained artifact and the fastest SQLite path, both
   measured, though neither margin is decisive on its own.
2. The strictest tooling for the risk that matters most: undocumented
   formats that change without notice.
3. Cross-compiles for the Linux adapters that will eventually exist.

Go would give faster compiles and a simpler language at the cost of a larger
binary and a pure-Go SQLite that is a touch slower. If the maintainer would
rather write Go, nothing measured here says no.

Bun is the fastest to write and shares a language with the opencode shim,
but the 63 MB artifact and the weaker strictness story make it the wrong
shape for a tool that is mostly parsing other programs' private state.

Swift's only unique advantage, the in-process bridge, measured at zero.

## What is not decided by this record

The distribution channel. Rust, TOML for configuration and snapshots, and
the command name `mysessions` are accepted, as recorded at the top.
