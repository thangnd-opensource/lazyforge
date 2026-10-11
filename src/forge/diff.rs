//! Parser for the unified diff text GitLab returns for each file.
//!
//! ```text
//! @@ -40,7 +40,8 @@ impl TokenStore
//!      fn refresh(&self) {
//! -        let t = self.fetch();
//! +        let _guard = self.lock.lock();
//! ```
//!
//! The `@@` header gives the first line number in the old and new file; after that, context
//! lines advance both counters, `-` lines only the old one and `+` lines only the new one.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    HunkHeader,
    Context,
    Added,
    Removed,
    /// For example `\ No newline at end of file`.
    Meta,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    /// The line without its leading `+`, `-` or space.
    pub text: String,
}

pub fn parse_unified_diff(diff: &str) -> Vec<DiffLine> {
    let mut old = 0;
    let mut new = 0;
    let mut lines = Vec::new();
    for raw in diff.lines() {
        if raw.starts_with("@@") {
            if let Some((old_start, new_start)) = parse_hunk_header(raw) {
                old = old_start;
                new = new_start;
            }
            lines.push(DiffLine {
                kind: DiffLineKind::HunkHeader,
                old_line: None,
                new_line: None,
                text: raw.to_string(),
            });
            continue;
        }
        // Slicing at 1 is safe: `+`, `-` and ` ` are one byte in UTF-8.
        let (kind, text) = match raw.as_bytes().first() {
            Some(b'+') => (DiffLineKind::Added, &raw[1..]),
            Some(b'-') => (DiffLineKind::Removed, &raw[1..]),
            Some(b' ') => (DiffLineKind::Context, &raw[1..]),
            _ => (DiffLineKind::Meta, raw),
        };
        let (old_line, new_line) = match kind {
            DiffLineKind::Added => {
                new += 1;
                (None, Some(new - 1))
            }
            DiffLineKind::Removed => {
                old += 1;
                (Some(old - 1), None)
            }
            DiffLineKind::Context => {
                old += 1;
                new += 1;
                (Some(old - 1), Some(new - 1))
            }
            _ => (None, None),
        };
        lines.push(DiffLine {
            kind,
            old_line,
            new_line,
            text: text.to_string(),
        });
    }
    lines
}

/// `@@ -12,7 +14,9 @@ optional context` → `(12, 14)`. The count after `,` is optional.
fn parse_hunk_header(line: &str) -> Option<(u32, u32)> {
    let mut parts = line.split_whitespace().skip(1);
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let start = |range: &str| range.split(',').next()?.parse().ok();
    Some((start(old)?, start(new)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use DiffLineKind::*;

    #[test]
    fn numbers_lines_from_the_hunk_header() {
        let diff = "@@ -10,3 +10,4 @@ fn main() {\n context\n-old\n+new\n+added\n tail\n";
        let lines = parse_unified_diff(diff);
        let summary: Vec<_> = lines
            .iter()
            .map(|l| (l.kind, l.old_line, l.new_line, l.text.as_str()))
            .collect();
        assert_eq!(
            summary,
            vec![
                (HunkHeader, None, None, "@@ -10,3 +10,4 @@ fn main() {"),
                (Context, Some(10), Some(10), "context"),
                (Removed, Some(11), None, "old"),
                (Added, None, Some(11), "new"),
                (Added, None, Some(12), "added"),
                (Context, Some(12), Some(13), "tail"),
            ]
        );
    }

    #[test]
    fn handles_several_hunks_and_single_line_ranges() {
        let diff = "@@ -1 +1 @@\n-a\n+b\n@@ -50,2 +50,2 @@\n x\n";
        let lines = parse_unified_diff(diff);
        assert_eq!(lines[1].old_line, Some(1));
        assert_eq!(lines[2].new_line, Some(1));
        assert_eq!(lines[4].old_line, Some(50));
    }

    #[test]
    fn keeps_meta_lines_and_unicode() {
        let diff = "@@ -1 +1 @@\n-héllo\n+xin chào\n\\ No newline at end of file\n";
        let lines = parse_unified_diff(diff);
        assert_eq!(lines[2].text, "xin chào");
        assert_eq!(lines[3].kind, Meta);
        assert_eq!(lines[3].new_line, None);
    }

    #[test]
    fn empty_diff_is_empty() {
        assert!(parse_unified_diff("").is_empty());
    }
}
