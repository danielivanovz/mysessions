//! Ghostty terminal adapter (macOS).
//!
//! Ghostty exposes windows, tabs, and terminal surfaces through the platform
//! scripting bridge, each with a stable id, and each surface with its working
//! directory and title. It exposes no tty or pid, so surfaces are paired to
//! sessions by directory and title elsewhere.
//!
//! Every Apple event costs about 20 ms regardless of language or process, so
//! the listing fetches nested collections once per window and iterates them
//! locally. Measured on 14 tabs: three events per tab cost about 550 ms; a
//! handful of events per window cost about 100 ms.

use super::{Surface, TerminalAdapter};
use anyhow::{Context, Result, bail};
use std::process::{Command, Output};

pub struct Ghostty;

/// Separators chosen to be unlikely in titles or paths. A title could still
/// contain them; the parser checks field counts and fails rather than guesses.
const FIELD: &str = "\u{1f}";
const RECORD: &str = "\u{1e}";

const LIST_SCRIPT: &str = r#"
if application "Ghostty" is not running then return ""
tell application "Ghostty"
    set out to ""
    repeat with w in windows
        set wid to id of w
        set tids to id of every tab of w
        set sids to id of every terminal of every tab of w
        set wds to working directory of every terminal of every tab of w
        set nms to name of every terminal of every tab of w
        repeat with ti from 1 to count of tids
            set tabSids to item ti of sids
            set tabWds to item ti of wds
            set tabNms to item ti of nms
            repeat with si from 1 to count of tabSids
                set out to out & wid & FS & (item ti of tids) & FS & (item si of tabSids) & FS & (item si of tabWds) & FS & (item si of tabNms) & RS
            end repeat
        end repeat
    end repeat
    return out
end tell
"#;

const OPEN_SCRIPT: &str = r#"
on run argv
    with timeout of 10 seconds
    tell application "Ghostty"
        set config to {initial working directory:item 1 of argv}
        if (count of windows) is 0 then
            set w to new window with configuration config
            set t to selected tab of w
        else
            set t to new tab in front window with configuration config
        end if
        set s to focused terminal of t
        input text (item 2 of argv) to s
        send key "enter" to s
        return id of s
    end tell
    end timeout
end run
"#;

impl TerminalAdapter for Ghostty {
    fn name(&self) -> &'static str {
        "ghostty"
    }

    fn list_surfaces(&self) -> Result<Vec<Surface>> {
        let script = format!(
            "set FS to (ASCII character 31)\nset RS to (ASCII character 30)\n{LIST_SCRIPT}"
        );
        let out = Command::new("osascript")
            .args(["-e", &script])
            .output()
            .context("running osascript")?;
        listing_from_output(out)
    }

    fn open_tab(&self, cwd: &std::path::Path, command: &str) -> Result<String> {
        // Dynamic values are argv, never interpolated into AppleScript. The
        // terminal receives a quoted shell command as one paste, then Enter.
        let out = Command::new("/usr/bin/osascript")
            .args(["-e", OPEN_SCRIPT])
            .arg(cwd)
            .arg(command)
            .output()
            .context("opening Ghostty tab")?;
        opened_surface_from_output(out)
    }
}

fn listing_from_output(out: Output) -> Result<Vec<Surface>> {
    if !out.status.success() {
        bail!(
            "osascript failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    parse_listing(&String::from_utf8(out.stdout)?)
}

fn opened_surface_from_output(out: Output) -> Result<String> {
    if !out.status.success() {
        bail!(
            "Ghostty tab creation/input failed: {} (a new tab may already exist)",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let id = String::from_utf8(out.stdout)?.trim().to_string();
    if id.is_empty() {
        bail!("Ghostty created a tab but returned no terminal id");
    }
    Ok(id)
}

fn parse_listing(text: &str) -> Result<Vec<Surface>> {
    let mut v = Vec::new();
    // osascript adds a final newline after the trailing record separator.
    // A newline before that separator is part of the title.
    for rec in text.trim_end_matches('\n').split(RECORD) {
        if rec.is_empty() {
            continue;
        }
        let f: Vec<&str> = rec.split(FIELD).collect();
        if f.len() != 5 {
            bail!("unexpected surface record with {} fields: {rec:?}", f.len());
        }
        v.push(Surface {
            window_id: f[0].to_string(),
            tab_id: f[1].to_string(),
            surface_id: f[2].to_string(),
            cwd: f[3].to_string(),
            title: f[4].to_string(),
        });
    }
    Ok(v)
}

/// Claude Code prefixes its tab title with a status glyph and a space. Strip
/// it so titles compare equal to the transcript's own title record.
pub fn strip_status_glyph(title: &str) -> &str {
    let mut chars = title.char_indices();
    match (chars.next(), chars.next()) {
        (Some((_, first)), Some((idx, ' '))) if !first.is_ascii() => &title[idx + 1..],
        _ => title,
    }
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
    fn property_listing_preserves_directory_and_title_text() {
        fn property(cwd: String, title: String) -> bool {
            // The protocol reserves these two separators; other characters,
            // including a newline at the end of a title, belong to the field.
            let cwd = cwd.replace(['\u{1e}', '\u{1f}'], "");
            let title = title.replace(['\u{1e}', '\u{1f}'], "");
            let text = format!("w{FIELD}t{FIELD}s{FIELD}{cwd}{FIELD}{title}{RECORD}\n");
            let surfaces = parse_listing(&text).unwrap();
            surfaces.len() == 1 && surfaces[0].cwd == cwd && surfaces[0].title == title
        }
        crate::test_support::check(property as fn(String, String) -> bool);
    }

    #[test]
    fn parses_records_and_fields() {
        let text = format!(
            "w{FIELD}t{FIELD}s{FIELD}/d{FIELD}Title{RECORD}w{FIELD}t2{FIELD}s2{FIELD}/e{FIELD}Two{RECORD}\n"
        );
        let v = parse_listing(&text).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[1].tab_id, "t2");
        assert_eq!(v[1].title, "Two");
    }

    #[test]
    fn rejects_malformed_record() {
        assert!(parse_listing(&format!("a{FIELD}b{RECORD}")).is_err());
    }

    #[test]
    fn newline_before_record_separator_belongs_to_the_title() {
        let text = format!("w{FIELD}t{FIELD}s{FIELD}/work{FIELD}\n{RECORD}\n");
        assert_eq!(parse_listing(&text).unwrap()[0].title, "\n");
    }

    #[test]
    fn strips_glyph_prefix_only() {
        assert_eq!(strip_status_glyph("✳ Review docs"), "Review docs");
        assert_eq!(strip_status_glyph("◐ Roost design"), "Roost design");
        assert_eq!(
            strip_status_glyph("just dashboard-prod"),
            "just dashboard-prod"
        );
        assert_eq!(strip_status_glyph("A B"), "A B");
    }

    #[test]
    fn osascript_failures_preserve_actionable_context() {
        let error = listing_from_output(output(1, b"", b"not authorised\n"))
            .unwrap_err()
            .to_string();
        assert_eq!(error, "osascript failed: not authorised");

        let error = opened_surface_from_output(output(1, b"", b"event timed out\n"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("event timed out"));
        assert!(error.contains("new tab may already exist"));
    }

    #[test]
    fn osascript_success_requires_valid_nonempty_output() {
        let text = format!("w{FIELD}t{FIELD}s{FIELD}/work{FIELD}Title{RECORD}\n");
        assert_eq!(
            listing_from_output(output(0, text.as_bytes(), b""))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            opened_surface_from_output(output(0, b"surface-1\n", b"")).unwrap(),
            "surface-1"
        );
        assert!(opened_surface_from_output(output(0, b" \n", b"")).is_err());
        assert!(listing_from_output(output(0, &[0xff], b"")).is_err());
        assert!(opened_surface_from_output(output(0, &[0xff], b"")).is_err());
    }
}
