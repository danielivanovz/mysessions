#!/usr/bin/env python3
"""Gate Mozilla's Rust cyclomatic metric; no local approximation of the AST."""

import json
import re
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ANALYZER = ROOT / "target/tools/bin/rust-code-analysis-cli"
LIMIT = 15


def run(*args):
    return subprocess.check_output([str(ANALYZER), *map(str, args)], cwd=ROOT, text=True)


def functions(space, file, parents=()):
    children = space["spaces"]
    name = space["name"] or "<anonymous>"
    if space["kind"] == "function":
        # RCA's sum includes nested closures/functions. Subtract their sums
        # and report each separately, so none are omitted or counted twice.
        score = space["metrics"]["cyclomatic"]["sum"] - sum(
            child["metrics"]["cyclomatic"]["sum"] for child in children
        )
        if score < 1 or not float(score).is_integer():
            raise ValueError(f"invalid cyclomatic score for {file}:{name}: {score}")
        yield {"file": file, "line": space["start_line"],
               "function": "::".join((*parents, name)), "cyclomatic": int(score)}
    for child in children:
        yield from functions(child, file, (*parents, name))


def error_nodes(path):
    output = run("--count", "ERROR", "--paths", path)
    match = re.search(r"^Found nodes: ([\d,]+)$", output, re.MULTILINE)
    if match is None:
        raise ValueError(f"unrecognized analyzer output: {output}")
    return int(match[1].replace(",", ""))


def main():
    if run("--version").strip() != "rust-code-analysis-cli 0.0.25":
        raise ValueError("expected rust-code-analysis-cli 0.0.25; rebuild with make tools")
    report_dir = ROOT / "target/complexity"
    report_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=report_dir) as work:
        work = Path(work)
        # Positive controls protect against empty output, parser drift, and
        # accidentally reporting an inclusive total as a per-function score.
        probe = work / "probe.rs"
        probe.write_text("fn straight() {}\nfn branch(b: bool) { if b {} }\n"
                         "fn outer() { let _f = |b: bool| { if b {} }; }\n"
                         "fn fallible() -> Option<()> { Some(())?; Some(()) }\n"
                         "fn pointer() { let mut x = 1; let _ = &raw mut x; }\n"
                         "fn chained(x: Option<bool>) { if let Some(b) = x && b {} }\n")
        run("--metrics", "--output-format", "json", "--output", work, "--paths", probe)
        probe_files = list(work.rglob("probe.rs.json"))
        if len(probe_files) != 1 or error_nodes(probe):
            raise ValueError("analyzer could not parse the Rust syntax probe")
        scores = sorted(row["cyclomatic"] for row in functions(json.loads(probe_files[0].read_text()), "probe"))
        if scores != [1, 1, 1, 2, 2, 2, 3]:
            raise ValueError(f"analyzer failed cyclomatic controls: {scores}")
        probe.write_text("fn broken( {\n")
        if error_nodes(probe) == 0:
            raise ValueError("analyzer failed to detect the invalid Rust control")
        if error_nodes("src"):
            raise ValueError("analyzer cannot parse src; metrics would be incomplete")
        run("--metrics", "--output-format", "json", "--output", work, "--paths", "src")
        rows = []
        sources = sorted((ROOT / "src").rglob("*.rs"))
        for source in sources:
            relative = source.relative_to(ROOT)
            report = work / f"{relative}.json"
            rows.extend(functions(json.loads(report.read_text()), str(relative)))
        if not rows:
            raise ValueError("analyzer reported no functions")
    rows.sort(key=lambda row: (-row["cyclomatic"], row["file"], row["line"]))
    (report_dir / "report.json").write_text(json.dumps(rows, indent=2) + "\n")
    print(f"Cyclomatic complexity: {len(rows)} functions/closures in {len(sources)} files; limit {LIMIT} (includes tests)")
    for row in rows[:15]:
        print(f"{row['cyclomatic']:3}  {row['file']}:{row['line']}  {row['function']}")
    print("Full report: target/complexity/report.json")
    violations = [row for row in rows if row["cyclomatic"] > LIMIT]
    if violations:
        raise SystemExit(f"FAIL: {len(violations)} functions exceed {LIMIT}")


if __name__ == "__main__":
    main()
