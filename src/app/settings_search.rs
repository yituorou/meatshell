//! Settings search (Settings panel › search box).
//!
//! The settings pages are hand-written Slint (`ui/interface_panel.slint`), with
//! every user-visible string written as `root.lang-en ? "English" : "中文"`.
//! Rather than keep a second, hand-maintained list of settings that would drift,
//! the index is built once at runtime from that very source (embedded with
//! `include_str!`): each `if root.ifd-page == "<id>"` section is one page, a
//! `label:` starts a setting row, the following `desc:` describes it, a
//! `SectionHeader` is its own entry, and every other string in the row (combo
//! options, button text, placeholders…) becomes a search keyword for it.
//! Both languages are always searchable, whichever one the UI shows.

use std::sync::OnceLock;

use slint::{ModelRc, SharedString, VecModel};

use super::*;

const SOURCE: &str = include_str!("../../ui/interface_panel.slint");
/// Cap on results shown at once.
const MAX_HITS: usize = 60;

#[derive(Debug, Clone, PartialEq)]
enum Kind {
    /// The page itself (matched by its nav title).
    Page,
    /// A `SectionHeader` group heading.
    Header,
    /// A `SettingRow` (or any other `label:`).
    Row,
}

#[derive(Debug, Clone)]
struct Entry {
    page: String,
    kind: Kind,
    label_en: String,
    label_zh: String,
    desc_en: String,
    desc_zh: String,
    /// Other strings belonging to this entry (both languages).
    keywords: Vec<String>,
}

#[derive(Debug, Default)]
struct Index {
    /// (page id, english title, chinese title), in nav order.
    pages: Vec<(String, String, String)>,
    entries: Vec<Entry>,
}

fn index() -> &'static Index {
    static INDEX: OnceLock<Index> = OnceLock::new();
    INDEX.get_or_init(|| build_index(SOURCE))
}

// ── Parsing ──────────────────────────────────────────────────────────────────

/// Parse a Slint string literal starting at `s[i] == '"'`. Returns the text
/// (interpolations `\{…}` dropped, escapes resolved) and the index just past
/// the closing quote.
fn parse_literal(s: &str, i: usize) -> Option<(String, usize)> {
    let b = s.as_bytes();
    if b.get(i) != Some(&b'"') {
        return None;
    }
    let mut out = String::new();
    let mut chars = s[i + 1..].char_indices();
    while let Some((off, c)) = chars.next() {
        match c {
            '"' => return Some((out, i + 1 + off + 1)),
            '\\' => match chars.next() {
                Some((_, '{')) => {
                    // Skip an interpolation, honouring nested braces.
                    let mut depth = 1;
                    for (_, c) in chars.by_ref() {
                        match c {
                            '{' => depth += 1,
                            '}' => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    out.push(' ');
                }
                Some((_, 'n')) => out.push(' '),
                Some((_, c)) => out.push(c),
                None => return None,
            },
            c => out.push(c),
        }
    }
    None
}

fn skip_ws(s: &str, mut i: usize) -> usize {
    let b = s.as_bytes();
    while i < b.len() && (b[i] as char).is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Parse either one literal or a `[ "a", "b", … ]` list at `i`.
fn parse_strings(s: &str, i: usize) -> Option<(Vec<String>, usize)> {
    let i = skip_ws(s, i);
    match s.as_bytes().get(i)? {
        b'"' => parse_literal(s, i).map(|(t, j)| (vec![t], j)),
        b'[' => {
            let mut out = Vec::new();
            let mut j = i + 1;
            loop {
                j = skip_ws(s, j);
                match s.as_bytes().get(j)? {
                    b']' => return Some((out, j + 1)),
                    b',' => j += 1,
                    b'"' => {
                        let (t, k) = parse_literal(s, j)?;
                        out.push(t);
                        j = k;
                    }
                    _ => return None,
                }
            }
        }
        _ => None,
    }
}

/// Parse `? <strings> : <strings>` right after a `root.lang-en` token.
fn parse_lang_pair(s: &str, i: usize) -> Option<(Vec<String>, Vec<String>, usize)> {
    let i = skip_ws(s, i);
    if s.as_bytes().get(i) != Some(&b'?') {
        return None;
    }
    let (en, j) = parse_strings(s, i + 1)?;
    let j = skip_ws(s, j);
    if s.as_bytes().get(j) != Some(&b':') {
        return None;
    }
    let (zh, k) = parse_strings(s, j + 1)?;
    Some((en, zh, k))
}

/// The property a value at `pos` is assigned to (`label`, `desc`, `text`…),
/// found as the last `ident:` in the current statement, plus whether that
/// statement sits directly inside a `SectionHeader { … }`.
fn property_at(s: &str, pos: usize) -> (String, bool) {
    let before = &s[..pos];
    let start = before.rfind([';', '{', '}']).map(|p| p + 1).unwrap_or(0);
    let stmt = &before[start..];
    let mut prop = String::new();
    let b = stmt.as_bytes();
    let mut k = 0;
    while k < b.len() {
        if b[k] == b':' {
            // Walk back over whitespace, then over an identifier.
            let mut e = k;
            while e > 0 && (b[e - 1] as char).is_ascii_whitespace() {
                e -= 1;
            }
            let mut st = e;
            while st > 0 && ((b[st - 1] as char).is_ascii_alphanumeric() || b[st - 1] == b'-') {
                st -= 1;
            }
            if st < e && (b[st] as char).is_ascii_alphabetic() {
                prop = stmt[st..e].to_string();
            }
        }
        k += 1;
    }
    let header = before[..start.saturating_sub(1)]
        .trim_end()
        .ends_with("SectionHeader");
    (prop, header)
}

fn clean(t: &str) -> String {
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn build_index(src: &str) -> Index {
    let mut idx = Index::default();

    // Nav titles: each `NavItem { … label: …; active: root.ifd-page == "<id>"`.
    for (pos, _) in src.match_indices("NavItem {") {
        // Look a bounded distance ahead, backing off to a char boundary (the
        // source is full of multi-byte Chinese text).
        let mut end = src.len().min(pos + 600);
        while !src.is_char_boundary(end) {
            end -= 1;
        }
        let chunk = &src[pos..end];
        let Some(end) = chunk.find("clicked") else {
            continue;
        };
        let chunk = &chunk[..end];
        let Some(p) = chunk.find("ifd-page == \"") else {
            continue;
        };
        let Some((id, _)) = parse_literal(chunk, p + "ifd-page == ".len()) else {
            continue;
        };
        let Some(l) = chunk.find("label:") else {
            continue;
        };
        let after = skip_ws(chunk, l + "label:".len());
        let (en, zh) = if chunk[after..].starts_with("root.lang-en") {
            match parse_lang_pair(chunk, after + "root.lang-en".len()) {
                Some((en, zh, _)) => (en.concat(), zh.concat()),
                None => continue,
            }
        } else {
            match parse_literal(chunk, after) {
                Some((t, _)) => (t.clone(), t),
                None => continue,
            }
        };
        idx.pages.push((id, clean(&en), clean(&zh)));
    }

    // Page sections, in source order.
    let marker = "if root.ifd-page == ";
    let starts: Vec<(usize, String)> = src
        .match_indices(marker)
        .filter_map(|(p, _)| parse_literal(src, p + marker.len()).map(|(id, _)| (p, id)))
        .collect();

    for (n, (start, page)) in starts.iter().enumerate() {
        let end = starts.get(n + 1).map(|s| s.0).unwrap_or(src.len());
        let sec = &src[*start..end];

        // (position, property, in SectionHeader, en strings, zh strings)
        let mut events: Vec<(usize, String, bool, Vec<String>, Vec<String>)> = Vec::new();
        for (p, _) in sec.match_indices("root.lang-en") {
            if let Some((en, zh, _)) = parse_lang_pair(sec, p + "root.lang-en".len()) {
                let (prop, header) = property_at(sec, p);
                events.push((p, prop, header, en, zh));
            }
        }
        // Plain, untranslated labels: `label: "SFTP";`.
        for (p, _) in sec.match_indices("label:") {
            let q = skip_ws(sec, p + "label:".len());
            if let Some((t, j)) = parse_literal(sec, q) {
                if sec[skip_ws(sec, j)..].starts_with(';') {
                    events.push((p, "label".into(), false, vec![t.clone()], vec![t]));
                }
            }
        }
        events.sort_by_key(|e| e.0);

        let mut cur: Option<usize> = None; // index into idx.entries
        for (_, prop, header, en, zh) in events {
            let (en, zh) = (clean(&en.join(" ")), clean(&zh.join(" ")));
            if en.is_empty() && zh.is_empty() {
                continue;
            }
            let new_entry = |kind: Kind| Entry {
                page: page.clone(),
                kind,
                label_en: en.clone(),
                label_zh: zh.clone(),
                desc_en: String::new(),
                desc_zh: String::new(),
                keywords: Vec::new(),
            };
            if header {
                idx.entries.push(new_entry(Kind::Header));
                cur = Some(idx.entries.len() - 1);
            } else if prop == "label" {
                idx.entries.push(new_entry(Kind::Row));
                cur = Some(idx.entries.len() - 1);
            } else if let Some(c) = cur {
                let e = &mut idx.entries[c];
                if prop == "desc" && e.kind == Kind::Row && e.desc_en.is_empty() {
                    e.desc_en = en;
                    e.desc_zh = zh;
                } else {
                    e.keywords.push(en);
                    e.keywords.push(zh);
                }
            }
        }
    }

    // Each page is also a result for its own title.
    for (id, en, zh) in &idx.pages {
        idx.entries.push(Entry {
            page: id.clone(),
            kind: Kind::Page,
            label_en: en.clone(),
            label_zh: zh.clone(),
            desc_en: String::new(),
            desc_zh: String::new(),
            keywords: Vec::new(),
        });
    }
    idx
}

// ── Search ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Hit {
    pub page: String,
    pub page_title: String,
    pub label: String,
    pub detail: String,
    /// Row label to scroll to / highlight ("" for pages and headers).
    pub focus: String,
}

fn contains_all(hay: &str, terms: &[String]) -> bool {
    terms.iter().all(|t| hay.contains(t.as_str()))
}

fn search_in(idx: &Index, query: &str, lang_en: bool, include_wsl: bool) -> Vec<Hit> {
    let terms: Vec<String> = query.split_whitespace().map(|t| t.to_lowercase()).collect();
    if terms.is_empty() {
        return Vec::new();
    }
    let title = |page: &str| {
        idx.pages
            .iter()
            .find(|p| p.0 == page)
            .map(|p| if lang_en { p.1.clone() } else { p.2.clone() })
            .unwrap_or_else(|| page.to_string())
    };
    let page_order = |page: &str| idx.pages.iter().position(|p| p.0 == page).unwrap_or(usize::MAX);

    let mut scored: Vec<(u8, usize, usize, Hit)> = Vec::new();
    for (n, e) in idx.entries.iter().enumerate() {
        if !include_wsl && e.page == "wsl" {
            continue;
        }
        let label = format!("{} {}", e.label_en, e.label_zh).to_lowercase();
        let desc = format!("{} {}", e.desc_en, e.desc_zh).to_lowercase();
        let kw = e.keywords.join(" ").to_lowercase();
        let all = format!("{label} {desc} {kw}");
        if !contains_all(&all, &terms) {
            continue;
        }
        // Rank: label hits first (pages, then headers, then rows), then
        // description hits, then keyword-only hits.
        let score = if contains_all(&label, &terms) {
            match e.kind {
                Kind::Page => 0,
                Kind::Header => 1,
                Kind::Row => 2,
            }
        } else if contains_all(&format!("{label} {desc}"), &terms) {
            3
        } else {
            4
        };
        let (l, d) = if lang_en {
            (&e.label_en, &e.desc_en)
        } else {
            (&e.label_zh, &e.desc_zh)
        };
        scored.push((
            score,
            page_order(&e.page),
            n,
            Hit {
                page: e.page.clone(),
                page_title: title(&e.page),
                label: l.clone(),
                detail: d.clone(),
                focus: if e.kind == Kind::Row { l.clone() } else { String::new() },
            },
        ));
    }
    scored.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));
    let mut seen = std::collections::HashSet::new();
    scored
        .into_iter()
        .map(|s| s.3)
        // Platform variants of one row (macOS / Windows wording) show once.
        .filter(|h| seen.insert((h.page.clone(), h.label.clone())))
        .take(MAX_HITS)
        .collect()
}

pub(crate) fn search(query: &str, lang_en: bool) -> Vec<Hit> {
    search_in(index(), query, lang_en, cfg!(windows))
}

/// Wire the settings search box.
pub(super) fn wire(window: &AppWindow) {
    window.on_search_settings(|query: SharedString, lang_en: bool| {
        let hits: Vec<SettingsHit> = search(query.as_str(), lang_en)
            .into_iter()
            .map(|h| SettingsHit {
                page: h.page.into(),
                page_title: h.page_title.into(),
                label: h.label.into(),
                detail: h.detail.into(),
                focus: h.focus.into(),
            })
            .collect();
        ModelRc::new(VecModel::from(hits))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_settings_source_is_indexed() {
        let idx = index();
        assert!(idx.pages.len() >= 15, "nav pages: {:?}", idx.pages);
        assert!(idx.pages.iter().any(|p| p.0 == "wallpaper" && p.2 == "壁纸与主题"));
        assert!(idx.entries.len() > 100, "entries: {}", idx.entries.len());
    }

    #[test]
    fn finds_rows_in_both_languages() {
        let en = search("cursor", true);
        assert!(en.iter().any(|h| h.page == "font" && h.focus == "Cursor shape"), "{en:?}");
        // Chinese query, English UI → still found, shown in English.
        let mixed = search("光标", true);
        assert!(mixed.iter().any(|h| h.focus == "Cursor shape"), "{mixed:?}");
        let zh = search("光标", false);
        assert!(zh.iter().any(|h| h.focus == "光标类型" && h.page_title == "字体"), "{zh:?}");
    }

    #[test]
    fn descriptions_and_options_are_searchable() {
        // Description text.
        let hits = search("Windows 中安装", false);
        assert!(hits.iter().any(|h| h.page == "font"), "{hits:?}");
        // Combo option text ("Frosted glass" is only a dropdown option).
        let hits = search("frosted", true);
        assert!(hits.iter().any(|h| h.focus == "Wallpaper display mode"), "{hits:?}");
    }

    #[test]
    fn page_titles_rank_first_and_terms_are_anded() {
        let hits = search("sftp", true);
        assert_eq!(hits.first().map(|h| h.page.as_str()), Some("sftp"));
        assert!(hits.first().unwrap().focus.is_empty());
        assert!(search("cursor zzzz-nope", true).is_empty());
        assert!(search("   ", true).is_empty());
    }

    #[test]
    fn literal_parser_handles_escapes_and_interpolation() {
        let s = r#""a \"b\" \{round(x * 100)}% c""#;
        assert_eq!(parse_literal(s, 0).unwrap().0, "a \"b\"  % c");
    }
}
