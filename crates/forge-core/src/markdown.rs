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

/// One block of a description, for clients that show text but no HTML (the launcher).
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Heading(String),
    Paragraph(String),
    /// A list item; `marker` is a bullet or `1.`.
    Item { marker: String, text: String },
    Code(String),
    Quote(String),
    /// Only from `image_base`, as on the website.
    Image { url: String, alt: String },
}

/// `md` as blocks of plain text: emphasis and links keep only their text, table rows become lines of
/// cells, images from elsewhere than `image_base` are dropped.
// ponytail: nested lists come out flat; indent by depth if descriptions use them.
pub fn blocks(md: &str, image_base: &str) -> Vec<Block> {
    #[derive(Clone)]
    enum Kind {
        Paragraph,
        Heading,
        Item(String),
        Code,
    }
    let image_base = format!("{}/", image_base.trim_end_matches('/'));
    let mut out = Vec::new();
    let (mut kind, mut text) = (Kind::Paragraph, String::new());
    let (mut lists, mut quotes) = (Vec::<Option<u64>>::new(), 0);
    let mut image: Option<(String, String)> = None;
    let (mut table, mut row, mut cell): (Option<Vec<String>>, Vec<String>, String) = (None, Vec::new(), String::new());
    let flush = |out: &mut Vec<Block>, kind: &Kind, text: &mut String, quotes: i32| {
        let t = std::mem::take(text);
        let t = if matches!(kind, Kind::Code) { t.trim_end().to_string() } else { t.trim().to_string() };
        if t.is_empty() {
            return;
        }
        out.push(match kind {
            Kind::Heading => Block::Heading(t),
            Kind::Item(marker) => Block::Item { marker: marker.clone(), text: t },
            Kind::Code => Block::Code(t),
            Kind::Paragraph if quotes > 0 => Block::Quote(t),
            Kind::Paragraph => Block::Paragraph(t),
        });
    };
    for event in Parser::new_ext(md, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH) {
        match event {
            Event::Start(Tag::Heading { .. }) => kind = Kind::Heading,
            Event::Start(Tag::CodeBlock(_)) => kind = Kind::Code,
            Event::Start(Tag::BlockQuote(_)) => quotes += 1,
            Event::End(TagEnd::BlockQuote(_)) => quotes -= 1,
            Event::Start(Tag::List(start)) => lists.push(start),
            Event::End(TagEnd::List(_)) => {
                lists.pop();
            }
            Event::Start(Tag::Item) => {
                flush(&mut out, &kind, &mut text, quotes);
                let marker = match lists.last_mut() {
                    Some(Some(n)) => {
                        *n += 1;
                        format!("{}.", *n - 1)
                    }
                    _ => "\u{2022}".to_string(),
                };
                kind = Kind::Item(marker);
            }
            // A second paragraph of an item stays in the item.
            Event::End(TagEnd::Paragraph) if matches!(kind, Kind::Item(_)) => text.push('\n'),
            Event::End(TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::CodeBlock | TagEnd::Item) => {
                flush(&mut out, &kind, &mut text, quotes);
                kind = Kind::Paragraph;
            }
            Event::Start(Tag::Table(_)) => table = Some(Vec::new()),
            Event::End(TagEnd::TableCell) => row.push(std::mem::take(&mut cell).trim().to_string()),
            Event::End(TagEnd::TableHead | TagEnd::TableRow) => {
                if let Some(rows) = table.as_mut() {
                    rows.push(std::mem::take(&mut row).join(" | "));
                }
            }
            Event::End(TagEnd::Table) => {
                if let Some(rows) = table.take() {
                    out.push(Block::Paragraph(rows.join("\n")));
                }
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                flush(&mut out, &kind, &mut text, quotes);
                image = Some((dest_url.to_string(), String::new()));
            }
            Event::End(TagEnd::Image) => {
                if let Some((url, alt)) = image.take().filter(|(url, _)| url.starts_with(&image_base)) {
                    out.push(Block::Image { url, alt });
                }
            }
            Event::Text(t) | Event::Code(t) => match (&mut image, &table) {
                (Some((_, alt)), _) => alt.push_str(&t),
                (None, Some(_)) => cell.push_str(&t),
                (None, None) => text.push_str(&t),
            },
            Event::SoftBreak | Event::HardBreak => text.push('\n'),
            _ => {}
        }
    }
    flush(&mut out, &kind, &mut text, quotes);
    out
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
    fn blocks_for_clients_without_html() {
        let md = "# Features\n\nShows **loot** and [chests](https://x.test).\nSecond line.\n\n- one\n- two\n\n1. first\n\n```\ncode()\n```\n\n> quote\n\n![shot](https://forge.test/img/a.webp) ![pixel](https://tracker.test/p.png)\n\n| a | b |\n|---|---|\n| 1 | 2 |";
        assert_eq!(
            blocks(md, IMG),
            [
                Block::Heading("Features".into()),
                Block::Paragraph("Shows loot and chests.\nSecond line.".into()),
                Block::Item { marker: "\u{2022}".into(), text: "one".into() },
                Block::Item { marker: "\u{2022}".into(), text: "two".into() },
                Block::Item { marker: "1.".into(), text: "first".into() },
                Block::Code("code()".into()),
                Block::Quote("quote".into()),
                Block::Image { url: "https://forge.test/img/a.webp".into(), alt: "shot".into() },
                Block::Paragraph("a | b\n1 | 2".into()),
            ]
        );
    }

    #[test]
    fn keeps_regular_markdown() {
        let out = r("# Title\n\n- one\n- two\n\n```\ncode()\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n> quote\n\n**bold** ~~gone~~");
        for tag in ["<h2>Title</h2>", "<li>one</li>", "<pre><code>code()", "<table>", "<td>1</td>", "<blockquote>", "<strong>bold</strong>", "<del>gone</del>"] {
            assert!(out.contains(tag), "missing {tag} in {out}");
        }
    }
}
