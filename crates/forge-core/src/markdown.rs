//! Addon descriptions: Markdown in, sanitized HTML out. The only place user text becomes HTML.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd, html};
use std::collections::HashSet;

const TAGS: &[&str] = &[
    "a", "blockquote", "br", "code", "del", "em", "h1", "h2", "h3", "h4", "h5", "h6", "hr", "img", "li", "ol", "p", "pre", "s",
    "strong", "table", "tbody", "td", "th", "thead", "tr", "ul",
];

/// Renders `md` to HTML that is safe to embed: no scripts, no event handlers, no styles, links only to
/// `http`, `https` and `mailto`, images only from `image_base` (the platform's own image URL).
/// A single line break stays one, as in GitHub comments; authors paste text, not prose. Headings go
/// one level down: the page around a description has its own h1.
pub fn render(md: &str, image_base: &str) -> String {
    let events = Parser::new_ext(md, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH).map(|e| match e {
        Event::SoftBreak => Event::HardBreak,
        Event::Start(Tag::Heading { level, id, classes, attrs }) => Event::Start(Tag::Heading { level: lower(level), id, classes, attrs }),
        Event::End(TagEnd::Heading(level)) => Event::End(TagEnd::Heading(lower(level))),
        e => e,
    });
    let mut raw = String::new();
    html::push_html(&mut raw, events);

    let image_base = format!("{}/", image_base.trim_end_matches('/'));
    ammonia::Builder::default()
        .tags(TAGS.iter().copied().collect::<HashSet<_>>())
        .generic_attributes(HashSet::new())
        .tag_attributes(
            [("a", ["href", "title"].into_iter().collect()), ("img", ["src", "alt", "title"].into_iter().collect())]
                .into_iter()
                .collect(),
        )
        // Code blocks keep their language, for syntax highlighting on the page.
        .allowed_classes([("code", ["language-js", "language-ts", "language-csharp", "language-rust", "language-python", "language-json"].into_iter().collect())].into_iter().collect())
        .url_schemes(["http", "https", "mailto"].into_iter().collect())
        .url_relative(ammonia::UrlRelative::Deny)
        .link_rel(Some("nofollow noopener noreferrer ugc"))
        .attribute_filter(move |element, attribute, value| {
            if element == "img" && attribute == "src" && !value.starts_with(&image_base) {
                return None;
            }
            Some(value.into())
        })
        .clean(&raw)
        .to_string()
}

fn lower(level: HeadingLevel) -> HeadingLevel {
    match level {
        HeadingLevel::H1 => HeadingLevel::H2,
        HeadingLevel::H2 => HeadingLevel::H3,
        HeadingLevel::H3 => HeadingLevel::H4,
        HeadingLevel::H4 => HeadingLevel::H5,
        _ => HeadingLevel::H6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IMG: &str = "https://forge.test/img";

    fn r(md: &str) -> String {
        render(md, IMG)
    }

    #[test]
    fn strips_scripts_and_handlers() {
        for evil in [
            "<script>alert(1)</script>",
            "<img src=x onerror=alert(1)>",
            "<svg onload=alert(1)>",
            "<iframe src=\"https://evil.test\"></iframe>",
            "<style>body{display:none}</style>",
            "<p style=\"position:fixed\">x</p>",
            "<!--><script>alert(1)</script>-->",
            "<a href=\"https://ok.test\" onclick=\"alert(1)\">x</a>",
            "<div onmouseover=alert(1)>x</div>",
        ] {
            let out = r(evil).to_lowercase();
            for bad in ["<script", "onerror", "onload", "onclick", "onmouseover", "<iframe", "<style", "style=", "<svg"] {
                assert!(!out.contains(bad), "{evil} -> {out}");
            }
        }
    }

    #[test]
    fn removes_dangerous_link_schemes() {
        for evil in [
            "[x](javascript:alert(1))",
            "<a href=\"jAvAsCrIpT:alert(1)\">x</a>",
            "<a href=\"jav&#x61;script:alert(1)\">x</a>",
            "<a href=\"data:text/html,<script>alert(1)</script>\">x</a>",
            "<a href=\"vbscript:msgbox(1)\">x</a>",
            "[x](/relative/path)",
        ] {
            let out = r(evil).to_lowercase();
            assert!(!out.contains("href"), "{evil} -> {out}");
        }
    }

    #[test]
    fn only_own_image_host() {
        let out = r("![a](https://tracker.test/pixel.png) <img src=\"https://tracker.test/2.png\">");
        assert!(!out.contains("tracker.test"), "{out}");
        let out = r("![shot](https://forge.test/img/abc.webp)");
        assert!(out.contains("src=\"https://forge.test/img/abc.webp\""), "{out}");
        assert!(!r("![a](https://forge.test/imgevil/x.png)").contains("imgevil"), "prefix must end at a slash");
    }

    #[test]
    fn links_get_rel() {
        let out = r("[site](https://example.test)");
        assert!(out.contains("rel=\"nofollow noopener noreferrer ugc\""), "{out}");
        assert!(out.contains("href=\"https://example.test\""), "{out}");
    }

    #[test]
    fn headings_sit_below_the_page_title() {
        // The page has its own h1 (the addon's name); a description's # is a section of it.
        let out = r("# Features\n\n## Setup\n\n###### Small");
        assert!(out.contains("<h2>Features</h2>") && out.contains("<h3>Setup</h3>") && out.contains("<h6>Small</h6>"), "{out}");
        assert!(!out.contains("<h1>"), "{out}");
    }

    #[test]
    fn a_single_line_break_stays_a_line_break() {
        let out = r("Golden chest pins.\nShows stack counts.");
        assert!(out.contains("Golden chest pins.<br>"), "{out}");
    }

    #[test]
    fn keeps_regular_markdown() {
        let out = r("# Title\n\n- one\n- two\n\n```\ncode()\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n> quote\n\n**bold** ~~gone~~");
        for tag in ["<h2>Title</h2>", "<li>one</li>", "<pre><code>code()", "<table>", "<td>1</td>", "<blockquote>", "<strong>bold</strong>", "<del>gone</del>"] {
            assert!(out.contains(tag), "missing {tag} in {out}");
        }
    }
}
