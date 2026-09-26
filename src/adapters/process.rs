//! Read-only process observations for adapters without a live-session registry.

use anyhow::{Context, Result, bail, ensure};
use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[derive(Debug, Clone)]
pub(super) struct Process {
    pub pid: u32,
    pub started_ms: i64,
    pub cwd: PathBuf,
    pub args: Vec<String>,
    pub files: HashSet<PathBuf>,
}

/// ps reports seconds, rounded down. Do not mistake this for an exact clock.
pub(super) fn utc_ms(text: &str, format: &str) -> Result<i64> {
    let text = CString::new(text)?;
    let format = CString::new(format)?;
    // SAFETY: libc::tm consists of integers and a nullable zone pointer; all
    // zero bits are valid. strptime initializes the fields described by format.
    let mut time: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both C strings outlive the call and time points to writable tm storage.
    let end = unsafe { libc::strptime(text.as_ptr(), format.as_ptr(), &raw mut time) };
    // SAFETY: a non-null strptime result points inside the still-live text C string.
    let complete = !end.is_null() && unsafe { *end == 0 };
    ensure!(complete, "unrecognised UTC time {text:?}");
    // SAFETY: time is an initialized tm and remains writable for the whole call.
    Ok(unsafe { libc::timegm(&raw mut time) } * 1000)
}

pub(super) fn terminal_processes(executable: &str) -> Result<Vec<Process>> {
    let mut processes = process_table(executable)?;
    if processes.is_empty() {
        return Ok(processes);
    }
    // One open-file query for all candidates. NUL fields preserve spaces and
    // newlines in paths. Codex's open rollout is the live process/session link.
    let pids = processes
        .iter()
        .map(|p| p.pid.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let out = Command::new("lsof")
        .args(["-nP", "-a", "-p", &pids, "-F0pfn"])
        .output()
        .context("reading agent open files")?;
    let observations = open_files_from_output(out)?;
    for process in &mut processes {
        if let Some((cwd, files)) = observations.get(&process.pid) {
            process.cwd = cwd.clone();
            process.files = files.clone();
            process.args = arguments(process.pid)?;
        }
    }
    // A process can exit between ps and lsof/argv. Recheck the start time so
    // a vanished process is harmless but a live, unreadable process is loud.
    let live = process_table(executable)?;
    processes.retain(|p| {
        live.iter()
            .any(|q| p.pid == q.pid && p.started_ms == q.started_ms)
    });
    for p in &processes {
        ensure!(
            !p.cwd.as_os_str().is_empty() && !p.args.is_empty(),
            "cannot read cwd/arguments for live {executable} pid {}",
            p.pid
        );
    }
    Ok(processes)
}

fn process_table(executable: &str) -> Result<Vec<Process>> {
    let out = Command::new("ps")
        .env("TZ", "UTC")
        .env("LC_ALL", "C")
        .args(["-ww", "-axo", "pid=,tty=,lstart=,comm="])
        .output()
        .context("reading process table")?;
    process_table_from_output(out, executable)
}

fn process_table_from_output(out: Output, executable: &str) -> Result<Vec<Process>> {
    ensure!(
        out.status.success(),
        "ps failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    parse_table(&String::from_utf8(out.stdout)?, executable)
}

fn open_files_from_output(out: Output) -> Result<OpenFiles> {
    // lsof exits 1 with no diagnostics when every candidate disappears
    // between ps and this query. Let the caller's liveness recheck discard
    // that race, but do not mistake an actual diagnostic for empty results.
    let diagnostic = String::from_utf8_lossy(&out.stderr);
    ensure!(
        out.status.success() || diagnostic.trim().is_empty(),
        "lsof failed: {}",
        diagnostic.trim()
    );
    parse_files(&String::from_utf8(out.stdout)?)
}

fn parse_table(text: &str, executable: &str) -> Result<Vec<Process>> {
    let mut result = Vec::new();
    for line in text.lines() {
        let mut rest = line.trim();
        let mut columns = Vec::new();
        for _ in 0..7 {
            let Some(end) = rest.find(char::is_whitespace) else {
                break;
            };
            columns.push(&rest[..end]);
            rest = rest[end..].trim_start();
        }
        if columns.len() != 7
            || Path::new(rest).file_name().and_then(|s| s.to_str()) != Some(executable)
        {
            continue;
        }
        if matches!(columns[1], "??" | "?") {
            continue;
        }
        result.push(Process {
            pid: columns[0].parse()?,
            started_ms: utc_ms(&columns[2..7].join(" "), "%a %b %e %T %Y")?,
            cwd: PathBuf::new(),
            args: Vec::new(),
            files: HashSet::new(),
        });
    }
    Ok(result)
}

type OpenFiles = HashMap<u32, (PathBuf, HashSet<PathBuf>)>;

fn parse_files(text: &str) -> Result<OpenFiles> {
    let mut result: OpenFiles = HashMap::new();
    let mut pid = None;
    let mut fd = "";
    for field in text.split('\0') {
        let field = field.trim_start_matches('\n');
        if let Some(value) = field.strip_prefix('p') {
            pid = Some(value.parse::<u32>()?);
            fd = "";
        } else if let Some(value) = field.strip_prefix('f') {
            fd = value;
        } else if let (Some(pid), Some(value)) = (pid, field.strip_prefix('n'))
            && Path::new(value).is_absolute()
        {
            let (cwd, files) = result.entry(pid).or_default();
            if fd == "cwd" {
                *cwd = value.into();
            } else {
                files.insert(value.into());
            }
        }
    }
    Ok(result)
}

/// Read argv as separate strings; ps's display loses boundaries in paths and
/// prompts containing spaces. Do not parse or retain the following environment.
#[cfg(target_os = "macos")]
fn arguments(pid: u32) -> Result<Vec<String>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, i32::try_from(pid)?];
    let mut size = 0;
    // SAFETY: mib has the three specified elements, size is writable, and
    // null oldp requests only the length. No kernel state is written.
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &raw mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return Ok(Vec::new()); // Liveness is rechecked by the caller.
    }
    ensure!(size <= 16 * 1024 * 1024, "unexpected process argument size");
    let mut buffer = vec![0u8; size];
    // SAFETY: buffer has the requested size writable bytes; sysctl receives
    // that capacity and updates size. mib and size remain live during the call.
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buffer.as_mut_ptr().cast(),
            &raw mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return Ok(Vec::new());
    }
    buffer.truncate(size);
    parse_arguments(&buffer)
}

#[cfg(not(target_os = "macos"))]
fn arguments(_pid: u32) -> Result<Vec<String>> {
    bail!("agent process discovery is currently supported on macOS only")
}

#[cfg(any(target_os = "macos", test))]
fn parse_arguments(buffer: &[u8]) -> Result<Vec<String>> {
    ensure!(buffer.len() >= 4, "truncated process arguments");
    let count = i32::from_ne_bytes(buffer[..4].try_into()?);
    ensure!(count > 0 && count < 100_000, "invalid argument count");
    let executable_end = buffer[4..]
        .iter()
        .position(|b| *b == 0)
        .context("missing executable terminator")?
        + 4;
    let mut pos = executable_end;
    while buffer.get(pos) == Some(&0) {
        pos += 1;
    }
    let mut result = Vec::new();
    for _ in 0..count {
        let Some(end) = buffer
            .get(pos..)
            .and_then(|b| b.iter().position(|b| *b == 0))
        else {
            bail!("truncated argv");
        };
        result.push(String::from_utf8(buffer[pos..pos + end].to_vec())?);
        pos += end + 1;
    }
    Ok(result)
}

#[cfg(test)]
// QuickCheck requires owned arguments so it can generate and shrink them.
#[allow(clippy::needless_pass_by_value)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    fn output(code: i32, stdout: &[u8], stderr: &[u8]) -> Output {
        Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
        }
    }

    #[test]
    fn property_argv_preserves_boundaries_and_never_reads_environment() {
        fn property(args: Vec<String>, environment: String) -> bool {
            let args: Vec<_> = std::iter::once("agent".to_string())
                .chain(args.into_iter().map(|s| s.replace('\0', "")))
                .collect();
            let mut buffer = i32::try_from(args.len()).unwrap().to_ne_bytes().to_vec();
            buffer.extend_from_slice(b"/bin/agent\0\0");
            for arg in &args {
                buffer.extend_from_slice(arg.as_bytes());
                buffer.push(0);
            }
            buffer.extend_from_slice(environment.as_bytes());
            parse_arguments(&buffer).unwrap() == args
        }
        crate::test_support::check(property as fn(Vec<String>, String) -> bool);
    }

    #[test]
    fn process_table_filters_by_executable_and_tty() {
        let table = " 21 ttys001 Sun Sep 6 19:40:32 2026 /a path/opencode\n22 ?? Sun Sep 6 19:40:32 2026 opencode\n23 ttys001 Sun Sep 6 19:40:32 2026 opencode-helper\n";
        let rows = parse_table(table, "opencode").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pid, 21);
        assert_eq!(rows[0].started_ms, 1_788_723_632_000);
        let files = parse_files("p21\0\nfcwd\0n/a path\0\nf4\0n/a\nfile.jsonl\0\n").unwrap();
        assert_eq!(files[&21].0, Path::new("/a path"));
        assert!(files[&21].1.contains(Path::new("/a\nfile.jsonl")));
    }

    #[test]
    fn argv_preserves_spaces_and_stops_before_environment() {
        let mut bytes = 4i32.to_ne_bytes().to_vec();
        bytes.extend_from_slice(
            b"/bin/opencode\0\0opencode\0/a path\0--prompt\0run tests\0PRIVATE=value\0",
        );
        assert_eq!(
            parse_arguments(&bytes).unwrap(),
            ["opencode", "/a path", "--prompt", "run tests"]
        );
        assert!(parse_arguments(&bytes[..15]).is_err());
    }

    #[test]
    fn subprocess_failures_are_not_mistaken_for_empty_observations() {
        let error = process_table_from_output(output(1, b"", b"permission denied\n"), "codex")
            .unwrap_err()
            .to_string();
        assert_eq!(error, "ps failed: permission denied");

        let error = open_files_from_output(output(1, b"", b"kernel denied access\n"))
            .unwrap_err()
            .to_string();
        assert_eq!(error, "lsof failed: kernel denied access");

        assert!(
            open_files_from_output(output(1, b"", b""))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn invalid_subprocess_encoding_is_reported() {
        assert!(process_table_from_output(output(0, &[0xff], b""), "codex").is_err());
        assert!(open_files_from_output(output(0, &[0xff], b"")).is_err());
    }
}
