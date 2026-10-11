use chrono::{DateTime, Utc};

/// ratatui draws a tab as a single cell (or nothing), which flattens indented code.
pub fn expand_tabs(text: &str) -> String {
    text.replace('\t', "    ")
}

/// Job duration: `45s`, `1m02s`, `1h03m`.
pub fn duration(seconds: f64) -> String {
    let seconds = seconds.max(0.0).round() as u64;
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m{:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h{:02}m", seconds / 3600, seconds % 3600 / 60),
    }
}

/// Short, lazygit-style age: `now`, `5m`, `3h`, `2d`, `4mo`, `1y`.
/// Shortens `text` to at most `max` characters by replacing its middle with `…`, so both
/// ends stay readable: `feature/very-long-name` → `feature…g-name`.
pub fn shorten_middle(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let keep = max - 1;
    let tail = keep / 2;
    let head = keep - tail;
    let start: String = text.chars().take(head).collect();
    let end: String = text.chars().skip(count - tail).collect();
    format!("{start}…{end}")
}

pub fn relative_time(time: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let seconds = (now - time).num_seconds().max(0);
    let minutes = seconds / 60;
    let hours = minutes / 60;
    let days = hours / 24;
    match () {
        _ if minutes < 1 => "now".to_string(),
        _ if hours < 1 => format!("{minutes}m"),
        _ if days < 1 => format!("{hours}h"),
        _ if days < 30 => format!("{days}d"),
        _ if days < 365 => format!("{}mo", days / 30),
        _ => format!("{}y", days / 365),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn shortens_the_middle() {
        use super::shorten_middle;
        assert_eq!(shorten_middle("develop", 10), "develop");
        assert_eq!(
            shorten_middle("feature/very-long-name", 14),
            "feature…g-name"
        );
        assert_eq!(
            shorten_middle("feature/very-long-name", 14).chars().count(),
            14
        );
        assert_eq!(shorten_middle("abc", 1), "…");
        assert_eq!(shorten_middle("abc", 0), "");
    }

    use super::*;
    use chrono::Duration;

    #[test]
    fn formats_each_unit() {
        let now = Utc::now();
        let ago = |d: Duration| relative_time(now - d, now);
        assert_eq!(ago(Duration::seconds(30)), "now");
        assert_eq!(ago(Duration::minutes(5)), "5m");
        assert_eq!(ago(Duration::hours(3)), "3h");
        assert_eq!(ago(Duration::days(2)), "2d");
        assert_eq!(ago(Duration::days(65)), "2mo");
        assert_eq!(ago(Duration::days(800)), "2y");
    }

    #[test]
    fn formats_durations() {
        assert_eq!(duration(4.6), "5s");
        assert_eq!(duration(62.0), "1m02s");
        assert_eq!(duration(3780.0), "1h03m");
        assert_eq!(duration(-1.0), "0s");
    }

    #[test]
    fn tabs_become_spaces() {
        assert_eq!(
            expand_tabs("\tif x {\n\t\treturn"),
            "    if x {\n        return"
        );
    }

    #[test]
    fn future_times_show_as_now() {
        let now = Utc::now();
        assert_eq!(relative_time(now + Duration::minutes(2), now), "now");
    }
}
