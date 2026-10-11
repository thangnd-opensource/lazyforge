//! Cleaning up raw job logs.
//!
//! GitLab runners write logs for a terminal: they keep ANSI colors (handled by the UI), but
//! also add things a log viewer should not show:
//!
//! - collapsible section markers: `section_start:1696000000:step_script\r\e[0K`
//!   (optionally with options, e.g. `section_start:…:name[collapsed=true]\r\e[0K`),
//! - `\r` used to redraw a line in place (progress bars), and the `\e[0K` "clear line" code.

const CLEAR_LINE: &str = "\x1b[0K";

/// Splits a raw job log into display lines, keeping ANSI color codes.
pub fn clean_trace(raw: &str) -> Vec<String> {
    let mut lines: Vec<String> = raw.split('\n').map(clean_line).collect();
    // A log ending with `\n` would leave an empty last line.
    if lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines
}

fn clean_line(line: &str) -> String {
    let line = strip_section_markers(line);
    // After a `\r` the terminal starts drawing at the beginning of the line again, so only
    // the last non-empty part is what a reader would see.
    let visible = line
        .split('\r')
        .rfind(|part| !part.replace(CLEAR_LINE, "").is_empty())
        .unwrap_or("");
    visible.replace(CLEAR_LINE, "")
}

fn strip_section_markers(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = find_marker(rest) {
        out.push_str(&rest[..start]);
        let marker = &rest[start..];
        // The marker ends at the `\r\e[0K` that follows it.
        match marker.find(&format!("\r{CLEAR_LINE}")) {
            Some(end) => rest = &marker[end + 1 + CLEAR_LINE.len()..],
            None => {
                // Not a well-formed marker: keep the text as it is.
                out.push_str(marker);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

fn find_marker(text: &str) -> Option<usize> {
    let start = text.find("section_start:");
    let end = text.find("section_end:");
    match (start, end) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_section_markers_and_keeps_colors() {
        let raw = "section_start:1696000000:prepare_executor\r\x1b[0K\x1b[0K\x1b[36;1mPreparing the \"docker\" executor\x1b[0;m\n\
                   Using docker image golang:1.25\n\
                   section_end:1696000005:prepare_executor\r\x1b[0K\n";
        assert_eq!(
            clean_trace(raw),
            vec![
                "\x1b[36;1mPreparing the \"docker\" executor\x1b[0;m",
                "Using docker image golang:1.25",
                "",
            ]
        );
    }

    #[test]
    fn handles_section_options_and_two_markers_on_one_line() {
        let raw =
            "section_end:1:a\r\x1b[0Ksection_start:2:b[collapsed=true]\r\x1b[0K$ go test ./...\n";
        assert_eq!(clean_trace(raw), vec!["$ go test ./..."]);
    }

    #[test]
    fn carriage_returns_keep_the_last_redraw() {
        let raw = "Downloading 10%\rDownloading 50%\rDownloading 100%\n";
        assert_eq!(clean_trace(raw), vec!["Downloading 100%"]);
        // A trailing `\r` (Windows line ending) does not blank the line.
        assert_eq!(clean_trace("ok\r\n"), vec!["ok"]);
    }

    #[test]
    fn malformed_markers_are_kept() {
        assert_eq!(
            clean_trace("echo section_start:oops"),
            vec!["echo section_start:oops"]
        );
    }

    #[test]
    fn empty_log_has_no_lines() {
        assert!(clean_trace("").is_empty());
    }
}
