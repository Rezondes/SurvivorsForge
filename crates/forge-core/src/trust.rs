//! Trust levels of an addon version, as the API reports them. The API decides them in one place
//! (the `version_trust` view); clients only read them.

use serde::{Deserialize, Serialize};

/// Highest priority first: a blocked version stays blocked whatever else is true.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trust {
    /// An admin blocked the version or the whole addon. Not installable.
    Blocked,
    /// The file on GitHub differs from the scanned one. Not installable.
    SourceChanged,
    /// An admin approved exactly this file.
    Verified,
    /// The automatic scan found something.
    Flagged,
    /// The automatic scan found nothing; no admin looked at it.
    Scanned,
    /// Not scanned yet, or the scan failed.
    Unscanned,
}

impl Trust {
    /// Blocked and changed files are never installed.
    pub fn installable(self) -> bool {
        !matches!(self, Trust::Blocked | Trust::SourceChanged)
    }

    /// Installing asks the user first.
    pub fn needs_confirmation(self) -> bool {
        matches!(self, Trust::Flagged | Trust::Unscanned)
    }

    /// The tag the website and the launcher show; a checked version of an author an admin verified
    /// before reads "Verified author".
    pub fn label(self, verified_author: bool) -> &'static str {
        match self {
            Trust::Verified => "Verified",
            Trust::Scanned if verified_author => "Verified author",
            Trust::Scanned => "Checked",
            Trust::Flagged => "Flagged",
            Trust::Unscanned => "Not checked",
            Trust::Blocked => "Blocked",
            Trust::SourceChanged => "File changed",
        }
    }

    /// The colour of that tag, the same in every client.
    pub fn tone(self, verified_author: bool) -> Tone {
        match self {
            Trust::Verified => Tone::Ok,
            Trust::Scanned if verified_author => Tone::Ok,
            Trust::Scanned => Tone::Plain,
            Trust::Unscanned => Tone::Warn,
            Trust::Flagged | Trust::Blocked | Trust::SourceChanged => Tone::Bad,
        }
    }
}

/// The colour of a trust tag: the theme's gold, or its ok, warn and bad colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Ok,
    Warn,
    Bad,
}

impl Tone {
    /// The website's CSS class (`badge ok`, `notice bad`); empty for plain.
    pub fn class(self) -> &'static str {
        match self {
            Tone::Plain => "",
            Tone::Ok => "ok",
            Tone::Warn => "warn",
            Tone::Bad => "bad",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_the_api() {
        let all = [Trust::Blocked, Trust::SourceChanged, Trust::Verified, Trust::Flagged, Trust::Scanned, Trust::Unscanned];
        let names: Vec<String> = all.iter().map(|t| serde_json::to_value(t).unwrap().as_str().unwrap().to_string()).collect();
        assert_eq!(names, ["blocked", "source_changed", "verified", "flagged", "scanned", "unscanned"]);
        assert!(!Trust::Blocked.installable() && !Trust::SourceChanged.installable() && Trust::Flagged.installable());
        assert!(Trust::Flagged.needs_confirmation() && Trust::Unscanned.needs_confirmation() && !Trust::Scanned.needs_confirmation());
    }

    #[test]
    fn every_client_names_and_colours_a_level_alike() {
        let shown = |t: Trust, author| (t.label(author), t.tone(author));
        assert_eq!(shown(Trust::Verified, false), ("Verified", Tone::Ok));
        assert_eq!(shown(Trust::Scanned, true), ("Verified author", Tone::Ok), "as green as Verified");
        assert_eq!(shown(Trust::Scanned, false), ("Checked", Tone::Plain));
        assert_eq!(shown(Trust::Flagged, false), ("Flagged", Tone::Bad));
        assert_eq!(shown(Trust::Blocked, false), ("Blocked", Tone::Bad));
        assert_eq!(shown(Trust::SourceChanged, false), ("File changed", Tone::Bad));
        assert_eq!(shown(Trust::Unscanned, false), ("Not checked", Tone::Warn));
        assert_eq!(Tone::Ok.class(), "ok");
    }
}
