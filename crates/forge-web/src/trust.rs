//! What the website says about each trust level. One place, so list badges, addon pages and the
//! download confirmation never disagree.

use forge_core::trust::Trust;

/// The notice shown with a version. `class` is one of the CSS notice classes.
#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub class: &'static str,
    pub badge: &'static str,
    pub text: &'static str,
    /// A download link exists at all.
    pub downloadable: bool,
    /// The download goes through the confirmation page first.
    pub confirm: bool,
}

pub fn notice(trust: &str, verified_author: bool, source_missing: bool) -> Notice {
    if source_missing && !matches!(trust, "blocked" | "source_changed") {
        return Notice {
            class: "",
            badge: "Unavailable",
            text: "The file is no longer available on GitHub.",
            downloadable: false,
            confirm: false,
        };
    }
    // Tag and colour come from forge-core, which the launcher uses too; an unknown level is not checked.
    let level: Trust = serde_json::from_value(serde_json::Value::from(trust)).unwrap_or(Trust::Unscanned);
    let text = match level {
        Trust::Verified => "An admin checked this exact file by hand.",
        Trust::Scanned if verified_author => "An admin verified an earlier version of this author; this one was checked automatically.",
        Trust::Scanned => "The automatic check found nothing. No admin has looked at it by hand.",
        Trust::Flagged => "The automatic check found something suspicious. Read the findings before you install it.",
        Trust::Blocked => "An admin blocked this version. It cannot be downloaded.",
        Trust::SourceChanged => "The file on GitHub was changed after it was checked. It cannot be downloaded until an admin looks at it.",
        Trust::Unscanned => "The automatic check has not run yet. Install it at your own risk.",
    };
    Notice {
        class: level.tone(verified_author).class(),
        badge: level.label(verified_author),
        text,
        downloadable: level.installable(),
        confirm: level.needs_confirmation(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels() {
        assert!(notice("verified", false, false).downloadable && !notice("verified", false, false).confirm);
        assert_eq!(notice("scanned", true, false).badge, "Verified author");
        assert_eq!(notice("scanned", true, false).class, notice("verified", false, false).class, "Verified author is as green as Verified");
        assert_eq!(notice("whatever", false, false).badge, "Not checked", "an unknown level reads as not checked");
        assert!(notice("flagged", false, false).confirm);
        assert!(notice("unscanned", false, false).confirm);
        assert!(!notice("blocked", false, false).downloadable);
        assert!(!notice("source_changed", false, false).downloadable);
        assert!(!notice("scanned", false, true).downloadable, "missing on GitHub");
        assert_eq!(notice("blocked", false, true).badge, "Blocked", "blocked wins over missing");
    }
}
