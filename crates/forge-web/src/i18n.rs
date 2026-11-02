//! Supported Azeroth Survivors UI locales.
//!
//! Locale codes are BCP 47-like identifiers. Keep the list in game menu order.
//! This module intentionally does not claim that all translations are complete.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Locale {
    pub code: &'static str,
    pub native_name: &'static str,
}

pub const DEFAULT_LOCALE: &str = "en";

pub const SUPPORTED: [Locale; 14] = [
    Locale { code: "en", native_name: "English" },
    Locale { code: "pl", native_name: "Polski" },
    Locale { code: "cs", native_name: "Čeština" },
    Locale { code: "de", native_name: "Deutsch" },
    Locale { code: "pt-BR", native_name: "Português (Brasil)" },
    Locale { code: "es", native_name: "Español" },
    Locale { code: "fr", native_name: "Français" },
    Locale { code: "ro", native_name: "Română" },
    Locale { code: "tr", native_name: "Türkçe" },
    Locale { code: "ru", native_name: "Русский" },
    Locale { code: "uk", native_name: "Українська" },
    Locale { code: "zh-Hans", native_name: "简体中文" },
    Locale { code: "ja", native_name: "日本語" },
    Locale { code: "ko", native_name: "한국어" },
];

/// Normalize an explicitly selected locale. Never echo unvalidated input into HTML.
pub fn normalize(raw: &str) -> Option<&'static str> {
    let candidate = raw.trim().replace('_', "-");
    if candidate.is_empty() { return None; }
    if let Some(locale) = SUPPORTED.iter().find(|locale| locale.code.eq_ignore_ascii_case(&candidate)) {
        return Some(locale.code);
    }
    // Language-only fallbacks, including the most common browser variants.
    let language = candidate.split('-').next()?;
    match language.to_ascii_lowercase().as_str() {
        "pt" => Some("pt-BR"),
        "zh" => Some("zh-Hans"),
        _ => SUPPORTED.iter().find(|locale| locale.code.eq_ignore_ascii_case(language)).map(|locale| locale.code),
    }
}

/// Honor quality weights in Accept-Language, preferring an explicitly selected locale.
pub fn negotiate(explicit: Option<&str>, accept_language: Option<&str>) -> &'static str {
    if let Some(selected) = explicit.and_then(normalize) { return selected; }
    let Some(header) = accept_language else { return DEFAULT_LOCALE; };
    let mut preferences: Vec<(usize, f32, &str)> = header.split(',').enumerate().filter_map(|(index, item)| {
        let mut fields = item.trim().split(';');
        let language = fields.next()?.trim();
        let mut quality = 1.0_f32;
        for field in fields {
            if let Some(value) = field.trim().strip_prefix("q=") {
                quality = value.parse::<f32>().ok()?;
            }
        }
        if !(0.0..=1.0).contains(&quality) || quality == 0.0 { return None; }
        Some((index, quality, language))
    }).collect();
    preferences.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    preferences.into_iter().find_map(|(_, _, value)| normalize(value)).unwrap_or(DEFAULT_LOCALE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_game_menu() {
        assert_eq!(SUPPORTED.len(), 14);
        assert_eq!(SUPPORTED[6].native_name, "Français");
        assert_eq!(SUPPORTED[11].code, "zh-Hans");
    }

    #[test]
    fn validates_and_normalizes() {
        assert_eq!(normalize("fr-FR"), Some("fr"));
        assert_eq!(normalize("pt_BR"), Some("pt-BR"));
        assert_eq!(normalize("zh-CN"), Some("zh-Hans"));
        assert_eq!(normalize("xx"), None);
        assert_eq!(normalize("<script>"), None);
    }

    #[test]
    fn negotiates_language_weights() {
        assert_eq!(negotiate(None, Some("en-US;q=0.7,fr-FR;q=0.9")), "fr");
        assert_eq!(negotiate(Some("ja"), Some("fr")), "ja");
        assert_eq!(negotiate(None, Some("xx,ko;q=0.5")), "ko");
        assert_eq!(negotiate(None, None), "en");
    }
}
