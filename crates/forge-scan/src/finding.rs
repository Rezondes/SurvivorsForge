use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Shown, but the zip still counts as clean.
    Info,
    /// Worth a look and shown like a warning, but the zip still counts as clean.
    Notice,
    /// Makes the zip "flagged".
    Warn,
    /// Makes the zip "flagged"; a strong sign of harm.
    Critical,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    /// Stable rule ID, e.g. `zip.executable`.
    pub rule: String,
    pub severity: Severity,
    /// Path inside the zip; empty for findings about the whole zip.
    pub file: String,
    pub line: Option<u32>,
    pub message: String,
    pub snippet: Option<String>,
}

pub const MAX_SNIPPET: usize = 200;

/// At most `MAX_SNIPPET` characters, control characters replaced, so it is safe to show.
pub fn clean_snippet(text: &str) -> String {
    text.trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_SNIPPET)
        .collect()
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub findings: Vec<Finding>,
}

impl Report {
    /// Any finding of `Warn` or `Critical`.
    pub fn flagged(&self) -> bool {
        self.findings.iter().any(|f| f.severity >= Severity::Warn)
    }

    pub fn count(&self, severity: Severity) -> usize {
        self.findings.iter().filter(|f| f.severity == severity).count()
    }

    /// One finding per rule, file and line: the most severe.
    pub(crate) fn dedupe(&mut self) {
        let mut kept: Vec<Finding> = Vec::new();
        for f in self.findings.drain(..) {
            match kept.iter_mut().find(|k| k.rule == f.rule && k.file == f.file && k.line == f.line && k.line.is_some()) {
                Some(k) if f.severity > k.severity => *k = f,
                Some(_) => {}
                None => kept.push(f),
            }
        }
        self.findings = kept;
    }

    pub(crate) fn add(&mut self, rule: &str, severity: Severity, file: &str, line: Option<u32>, message: impl Into<String>) {
        self.findings.push(Finding { rule: rule.into(), severity, file: file.into(), line, message: message.into(), snippet: None });
    }

    pub(crate) fn add_with_snippet(
        &mut self,
        rule: &str,
        severity: Severity,
        file: &str,
        line: Option<u32>,
        message: impl Into<String>,
        snippet: &str,
    ) {
        self.add(rule, severity, file, line, message);
        self.findings.last_mut().unwrap().snippet = Some(clean_snippet(snippet));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_is_bounded_and_clean() {
        let s = clean_snippet(&format!("a\u{1b}[31mb\0c{}", "x".repeat(500)));
        assert_eq!(s.chars().count(), MAX_SNIPPET);
        assert!(!s.chars().any(char::is_control));
        assert!(s.starts_with("a [31mb c"));
    }

    #[test]
    fn info_alone_is_not_flagged() {
        let mut report = Report::default();
        report.add("x", Severity::Info, "", None, "m");
        assert!(!report.flagged());
        report.add("y", Severity::Warn, "", None, "m");
        assert!(report.flagged());
    }
}
