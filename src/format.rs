//! Turns kubectl output into something to lay out: log lines (JSON or not) with their time,
//! level and message; `kubectl describe` as fields and sections; and colour spans.

use rune::{Color, Span};
use rune_kit::Kit;
use rune_kit::syntax::{SyntaxTheme, highlight};
use serde_json::Value;

/// One log line, with what a structured logger put in it pulled out.
#[derive(Debug, Clone, PartialEq)]
pub struct LogLine {
    /// `pod/name/container` when kubectl prefixed the line (logs across pods).
    pub prefix: String,
    pub time: String,
    pub level: String,
    pub msg: String,
    /// The parsed object for JSON lines, shown pretty when expanded.
    pub json: Option<Value>,
    /// Fields besides time, level and message.
    pub extra: usize,
}

const TIME_KEYS: &[&str] = &[
    "time",
    "timestamp",
    "@timestamp",
    "ts",
    "t",
    "@t",
    "asctime",
    "date",
];
const LEVEL_KEYS: &[&str] = &[
    "level",
    "severity",
    "lvl",
    "levelname",
    "log.level",
    "@l",
    "loglevel",
];
const MSG_KEYS: &[&str] = &["message", "msg", "@m", "@mt", "event", "log", "text"];

pub fn log_line(raw: &str) -> LogLine {
    let (prefix, body) = match raw.strip_prefix('[').and_then(|r| r.split_once("] ")) {
        Some((p, rest)) if !p.contains(' ') => (p.to_string(), rest),
        _ => (String::new(), raw),
    };
    let trimmed = body.trim_start();
    if trimmed.starts_with('{')
        && let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(trimmed)
    {
        let m = v.as_object().expect("object");
        let pick = |keys: &[&str]| {
            keys.iter().find_map(|k| m.get(*k)).map(|x| match x {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
        };
        let time = pick(TIME_KEYS).map(|t| short_time(&t)).unwrap_or_default();
        let level = pick(LEVEL_KEYS).unwrap_or_default();
        let msg = pick(MSG_KEYS).unwrap_or_default();
        let known = [TIME_KEYS, LEVEL_KEYS, MSG_KEYS];
        let extra = m
            .keys()
            .filter(|k| !known.iter().any(|ks| ks.contains(&k.as_str())))
            .count();
        return LogLine {
            prefix,
            time,
            level,
            msg,
            json: Some(v),
            extra,
        };
    }
    LogLine {
        prefix,
        time: String::new(),
        level: text_level(body).to_string(),
        msg: body.to_string(),
        json: None,
        extra: 0,
    }
}

/// `2026-11-10T22:41:52.777828+00:00` → `22:41:52.777`.
fn short_time(t: &str) -> String {
    let Some((_, rest)) = t.split_once('T') else {
        return t.to_string();
    };
    let end = rest.find(['Z', '+', '-']).unwrap_or(rest.len());
    let clock = &rest[..end];
    match clock.split_once('.') {
        Some((hms, frac)) => format!("{hms}.{}", &frac[..frac.len().min(3)]),
        None => clock.to_string(),
    }
}

/// A level word near the start of a plain text line.
fn text_level(line: &str) -> &'static str {
    let head: String = line.chars().take(60).collect::<String>().to_uppercase();
    for (word, level) in [
        ("FATAL", "fatal"),
        ("PANIC", "fatal"),
        ("ERROR", "error"),
        ("ERR ", "error"),
        ("WARN", "warn"),
        ("INFO", "info"),
        ("DEBUG", "debug"),
        ("TRACE", "debug"),
    ] {
        if head.contains(word) {
            return level;
        }
    }
    ""
}

pub fn level_color(k: &Kit, level: &str) -> Color {
    match level.to_lowercase().as_str() {
        "fatal" | "panic" | "critical" | "crit" | "error" | "err" | "emergency" | "alert" => {
            k.c.danger.fg
        }
        "warn" | "warning" => k.c.warning.fg,
        "info" | "information" | "notice" => k.c.text_accent,
        "" => k.c.text_base,
        _ => k.c.text_faint,
    }
}

/// One top-level item of `kubectl describe`: a field (`Name: web`), a field whose value runs
/// over several lines (`Labels:`), or a section of indented lines (`Containers:`).
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub key: String,
    pub value: String,
    /// Indented lines that follow, dedented by the entry's indentation.
    pub lines: Vec<String>,
}

impl Entry {
    /// A field with one value, or several `a=b` ones (labels, selectors).
    pub fn is_field(&self) -> bool {
        self.lines.is_empty()
            || (!self.value.is_empty() && self.lines.iter().all(|l| !l.starts_with(' ')))
    }
}

pub fn describe(text: &str) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    for line in text.lines() {
        let indented = line.starts_with(' ') || line.starts_with('\t');
        if !indented && let Some((key, value)) = line.split_once(':') {
            out.push(Entry {
                key: key.trim().to_string(),
                value: value.trim().to_string(),
                lines: Vec::new(),
            });
            continue;
        }
        match out.last_mut() {
            Some(e) => e.lines.push(line.to_string()),
            None => out.push(Entry {
                key: String::new(),
                value: line.to_string(),
                lines: Vec::new(),
            }),
        }
    }
    for e in &mut out {
        while e.lines.last().is_some_and(|l| l.trim().is_empty()) {
            e.lines.pop();
        }
        // Continuation lines of a field line up under its value; sections indent by two.
        let indent = e
            .lines
            .iter()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.len() - l.trim_start().len())
            .min()
            .unwrap_or(0);
        for l in &mut e.lines {
            *l = l.get(indent..).unwrap_or("").to_string();
        }
    }
    out
}

/// Colours for a `key: value` line: the key muted, status words by meaning.
pub fn kv_spans(k: &Kit, line: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let lead = line.len() - line.trim_start().len();
    let rest = &line[lead..];
    let key_end = rest
        .find(":  ")
        .or_else(|| rest.find(": "))
        .or_else(|| rest.ends_with(':').then(|| rest.len() - 1))
        .filter(|i| !rest[..*i].contains("  ") && !rest[..*i].starts_with('/'));
    if let Some(i) = key_end {
        spans.push(Span::new(lead, lead + i + 1).color(k.c.text_muted));
    }
    for (word, color) in [
        ("Running", k.c.success.fg),
        ("True", k.c.success.fg),
        ("Completed", k.c.success.fg),
        ("Normal", k.c.text_muted),
        ("False", k.c.danger.fg),
        ("Failed", k.c.danger.fg),
        ("Error", k.c.danger.fg),
        ("CrashLoopBackOff", k.c.danger.fg),
        ("OOMKilled", k.c.danger.fg),
        ("ImagePullBackOff", k.c.danger.fg),
        ("ErrImagePull", k.c.danger.fg),
        ("Unhealthy", k.c.danger.fg),
        ("Warning", k.c.warning.fg),
        ("Waiting", k.c.warning.fg),
        ("Pending", k.c.warning.fg),
        ("Terminated", k.c.text_faint),
        ("<none>", k.c.text_faint),
    ] {
        let mut from = key_end.map_or(0, |i| lead + i + 1);
        while let Some(pos) = line[from..].find(word) {
            let (a, b) = (from + pos, from + pos + word.len());
            let edge = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
            if edge(line[..a].chars().last()) && edge(line[b..].chars().next()) {
                spans.push(Span::new(a, b).color(color).bold());
            }
            from = b;
        }
    }
    spans.sort_by_key(|s| s.start);
    spans
}

/// Syntax colours for code in `lang` (yaml, json), from the kit's theme.
pub fn code_spans(k: &Kit, lang: &str, code: &str) -> Vec<Span> {
    SyntaxTheme::from_kit(k).spans(&highlight(Some(lang), code), 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_and_plain_logs() {
        let l = log_line(
            r#"[pod/web-1/api] {"time":"2026-11-10T22:41:52.777828+00:00","severity":"ERROR","message":"boom","path":"/x"}"#,
        );
        assert_eq!(
            (
                l.prefix.as_str(),
                l.time.as_str(),
                l.level.as_str(),
                l.msg.as_str(),
                l.extra
            ),
            ("pod/web-1/api", "22:41:52.777", "ERROR", "boom", 1)
        );
        let p = log_line("2026/11/10 WARN disk almost full");
        assert!(p.json.is_none());
        assert_eq!(p.level, "warn");
    }

    #[test]
    fn describe_sections() {
        let d = describe(
            "Name:         web\nLabels:       app=web\n              tier=api\nContainers:\n  api:\n    Image:  web:1\n\nEvents:  <none>\n",
        );
        let keys: Vec<&str> = d.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys, ["Name", "Labels", "Containers", "Events"]);
        assert!(d[1].is_field() && d[1].lines == ["tier=api"]);
        assert!(!d[2].is_field() && d[2].lines == ["api:", "  Image:  web:1"]);
    }
}
