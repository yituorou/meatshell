//! Terminal color scheme system (`iTerm2-Color-Schemes` integration + `ThemeManager`).
//!
//! Provides:
//! - Unified `TerminalTheme` model with metadata (`theme_id`, `name`, `author`,
//!   `license`, `source`, `is_dark`), core terminal colors (`background`,
//!   `foreground`, `cursor`, `cursor_text`, `selection`, `selection_text`, `bold`),
//!   and strict ANSI 16-color slots (`0..=7` normal + `8..=15` bright).
//! - Native `.itermcolors` XML Plist parser and exporter (no extra crate dependencies).
//! - Built-in catalog of 915+ community schemes from `mbadolato/iTerm2-Color-Schemes`.
//! - `ThemeManager` for searching, filtering (All / Dark / Light / Favorites / Custom),
//!   live previewing, switching, favoring, and importing/exporting themes.
//! - `ResolvedTerminalPalette` (`Copy` struct) consumed directly by `TermBuffer`
//!   and `presentation.rs` for zero-allocation per-span color resolution.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const DEFAULT_THEME_ID: &str = "default";
pub const ITERM2_LICENSE: &str = "MIT";
pub const ITERM2_REPO_URL: &str = "https://github.com/mbadolato/iTerm2-Color-Schemes";

/// Standard dark ANSI 16 fallback palette (0..=7 normal, 8..=15 bright).
pub const DEFAULT_ANSI16_DARK_HEX: [&str; 16] = [
    "#000000", // 0  black
    "#CD3131", // 1  red
    "#0DBC79", // 2  green
    "#E5E510", // 3  yellow
    "#2472C8", // 4  blue
    "#BC3FBC", // 5  magenta
    "#11A8CD", // 6  cyan
    "#E5E5E5", // 7  white
    "#666666", // 8  bright black
    "#F14C4C", // 9  bright red
    "#23D18B", // 10 bright green
    "#F5F543", // 11 bright yellow
    "#3B8EEA", // 12 bright blue
    "#D670D6", // 13 bright magenta
    "#29B8DB", // 14 bright cyan
    "#FFFFFF", // 15 bright white
];

/// Standard light ANSI 16 fallback palette (0..=7 normal, 8..=15 bright).
pub const DEFAULT_ANSI16_LIGHT_HEX: [&str; 16] = [
    "#1C1C1E", // 0  black
    "#C0392B", // 1  red
    "#1A7F37", // 2  green
    "#856404", // 3  yellow
    "#0451A5", // 4  blue
    "#800080", // 5  magenta
    "#0E725C", // 6  cyan
    "#3A3A3C", // 7  white (dark grey for readability on light bg)
    "#555555", // 8  bright black
    "#E74C3C", // 9  bright red
    "#27AE60", // 10 bright green
    "#D4AC0D", // 11 bright yellow
    "#2E86C1", // 12 bright blue
    "#9B59B6", // 13 bright magenta
    "#1ABC9C", // 14 bright cyan
    "#2C2C2E", // 15 bright white
];

/// Named 8-color ANSI group used for structured JSON export/import compatibility.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ansi8Colors {
    pub black: String,
    pub red: String,
    pub green: String,
    pub yellow: String,
    pub blue: String,
    pub magenta: String,
    pub cyan: String,
    pub white: String,
}

/// Named normal + bright ANSI 16-color representation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnsiPaletteNamed {
    pub normal: Ansi8Colors,
    pub bright: Ansi8Colors,
}

impl AnsiPaletteNamed {
    pub fn from_array(ansi: &[String; 16]) -> Self {
        Self {
            normal: Ansi8Colors {
                black: ansi[0].clone(),
                red: ansi[1].clone(),
                green: ansi[2].clone(),
                yellow: ansi[3].clone(),
                blue: ansi[4].clone(),
                magenta: ansi[5].clone(),
                cyan: ansi[6].clone(),
                white: ansi[7].clone(),
            },
            bright: Ansi8Colors {
                black: ansi[8].clone(),
                red: ansi[9].clone(),
                green: ansi[10].clone(),
                yellow: ansi[11].clone(),
                blue: ansi[12].clone(),
                magenta: ansi[13].clone(),
                cyan: ansi[14].clone(),
                white: ansi[15].clone(),
            },
        }
    }

    pub fn to_array(&self) -> [String; 16] {
        [
            self.normal.black.clone(),
            self.normal.red.clone(),
            self.normal.green.clone(),
            self.normal.yellow.clone(),
            self.normal.blue.clone(),
            self.normal.magenta.clone(),
            self.normal.cyan.clone(),
            self.normal.white.clone(),
            self.bright.black.clone(),
            self.bright.red.clone(),
            self.bright.green.clone(),
            self.bright.yellow.clone(),
            self.bright.blue.clone(),
            self.bright.magenta.clone(),
            self.bright.cyan.clone(),
            self.bright.white.clone(),
        ]
    }
}

/// Unified terminal color scheme data structure.
///
/// ANSI slot indices strictly follow the ECMA-48 / xterm standard:
/// - `0`: Black, `1`: Red, `2`: Green, `3`: Yellow, `4`: Blue, `5`: Magenta, `6`: Cyan, `7`: White
/// - `8`: Bright Black, `9`: Bright Red, `10`: Bright Green, `11`: Bright Yellow,
///   `12`: Bright Blue, `13`: Bright Magenta, `14`: Bright Cyan, `15`: Bright White
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalTheme {
    pub theme_id: String,
    pub name: String,
    #[serde(default = "default_author")]
    pub author: String,
    #[serde(default = "default_license")]
    pub license: String,
    #[serde(default = "default_source")]
    pub source: String,
    #[serde(default = "default_true")]
    pub is_dark: bool,
    pub background: String,
    pub foreground: String,
    #[serde(default)]
    pub cursor: String,
    #[serde(default)]
    pub cursor_text: String,
    #[serde(default)]
    pub selection: String,
    #[serde(default)]
    pub selection_text: String,
    #[serde(default)]
    pub bold: String,
    pub ansi: [String; 16],
    /// Optional raw key-value snapshot when imported from a partial external file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_colors: Option<HashMap<String, String>>,
}

fn default_author() -> String {
    "iTerm2-Color-Schemes Contributors".to_string()
}

fn default_license() -> String {
    ITERM2_LICENSE.to_string()
}

fn default_source() -> String {
    ITERM2_REPO_URL.to_string()
}

fn default_true() -> bool {
    true
}

impl TerminalTheme {
    /// Built-in adaptive default theme that follows MeatShell's light/dark workbench mode.
    pub fn default_for_mode(is_dark: bool) -> Self {
        if is_dark {
            Self {
                theme_id: DEFAULT_THEME_ID.to_string(),
                name: "MeatShell Default".to_string(),
                author: "MeatShell".to_string(),
                license: "MIT".to_string(),
                source: "built-in".to_string(),
                is_dark: true,
                background: "#212121".to_string(),
                foreground: "#D4D4D4".to_string(),
                cursor: "#D4D4D4".to_string(),
                cursor_text: "#212121".to_string(),
                selection: "#3B82F6".to_string(),
                selection_text: "#FFFFFF".to_string(),
                bold: "#FFFFFF".to_string(),
                ansi: DEFAULT_ANSI16_DARK_HEX.map(str::to_string),
                raw_colors: None,
            }
        } else {
            Self {
                theme_id: DEFAULT_THEME_ID.to_string(),
                name: "MeatShell Default".to_string(),
                author: "MeatShell".to_string(),
                license: "MIT".to_string(),
                source: "built-in".to_string(),
                is_dark: false,
                background: "#FFFFFF".to_string(),
                foreground: "#2D2D2F".to_string(),
                cursor: "#2D2D2F".to_string(),
                cursor_text: "#FFFFFF".to_string(),
                selection: "#2563EB".to_string(),
                selection_text: "#FFFFFF".to_string(),
                bold: "#1C1C1E".to_string(),
                ansi: DEFAULT_ANSI16_LIGHT_HEX.map(str::to_string),
                raw_colors: None,
            }
        }
    }

    /// Return the named normal + bright ANSI 16 palette structure.
    pub fn ansi_palette_named(&self) -> AnsiPaletteNamed {
        AnsiPaletteNamed::from_array(&self.ansi)
    }

    /// Ensure all optional fields (`cursor`, `selection`, `bold`, and any malformed hex)
    /// have valid `#RRGGBB` values, filling missing ones from sensible defaults.
    pub fn normalized(mut self) -> Self {
        let bg_rgb = parse_hex_rgb(&self.background).unwrap_or((0x21, 0x21, 0x21));
        self.is_dark = is_rgb_dark(bg_rgb.0, bg_rgb.1, bg_rgb.2);
        self.background = rgb_to_hex(bg_rgb);

        let default_fg = if self.is_dark {
            (0xD4, 0xD4, 0xD4)
        } else {
            (0x2D, 0x2D, 0x2F)
        };
        let fg_rgb = parse_hex_rgb(&self.foreground).unwrap_or(default_fg);
        self.foreground = rgb_to_hex(fg_rgb);

        let cursor_rgb = parse_hex_rgb(&self.cursor).unwrap_or(fg_rgb);
        self.cursor = rgb_to_hex(cursor_rgb);

        let cursor_text_rgb = parse_hex_rgb(&self.cursor_text).unwrap_or(bg_rgb);
        self.cursor_text = rgb_to_hex(cursor_text_rgb);

        let default_sel = if self.is_dark {
            (0x3B, 0x4E, 0x6B)
        } else {
            (0xB4, 0xD5, 0xFE)
        };
        let sel_rgb = parse_hex_rgb(&self.selection).unwrap_or(default_sel);
        self.selection = rgb_to_hex(sel_rgb);

        let sel_text_rgb = parse_hex_rgb(&self.selection_text).unwrap_or(fg_rgb);
        self.selection_text = rgb_to_hex(sel_text_rgb);

        let bold_rgb = parse_hex_rgb(&self.bold).unwrap_or(fg_rgb);
        self.bold = rgb_to_hex(bold_rgb);

        let fallback_ansi = if self.is_dark {
            &DEFAULT_ANSI16_DARK_HEX
        } else {
            &DEFAULT_ANSI16_LIGHT_HEX
        };
        for (i, slot) in self.ansi.iter_mut().enumerate() {
            let rgb = parse_hex_rgb(slot)
                .or_else(|| parse_hex_rgb(fallback_ansi[i]))
                .unwrap_or(fg_rgb);
            *slot = rgb_to_hex(rgb);
        }

        if self.theme_id.trim().is_empty() {
            self.theme_id = slugify_theme_id(&self.name);
        }
        if self.author.trim().is_empty() {
            self.author = default_author();
        }
        if self.license.trim().is_empty() {
            self.license = default_license();
        }
        if self.source.trim().is_empty() {
            self.source = default_source();
        }
        self
    }

    /// Resolve into a compact `Copy` palette for the terminal renderer.
    pub fn to_resolved_palette(&self) -> ResolvedTerminalPalette {
        if self.theme_id == DEFAULT_THEME_ID {
            return ResolvedTerminalPalette::default_for_mode(self.is_dark);
        }
        let bg = parse_hex_rgb(&self.background).unwrap_or((0x21, 0x21, 0x21));
        let is_dark = is_rgb_dark(bg.0, bg.1, bg.2);
        let default_fg = if is_dark {
            (0xD4, 0xD4, 0xD4)
        } else {
            (0x2D, 0x2D, 0x2F)
        };
        let fg = parse_hex_rgb(&self.foreground).unwrap_or(default_fg);
        let bg_lum = relative_luminance(bg.0, bg.1, bg.2);
        let raw_cursor = parse_hex_rgb(&self.cursor).unwrap_or(fg);
        let cursor = if (relative_luminance(raw_cursor.0, raw_cursor.1, raw_cursor.2) - bg_lum).abs() < 0.06 {
            fg
        } else {
            raw_cursor
        };
        let cursor_lum = relative_luminance(cursor.0, cursor.1, cursor.2);
        let raw_cursor_text = parse_hex_rgb(&self.cursor_text).unwrap_or(bg);
        let cursor_text = if (relative_luminance(raw_cursor_text.0, raw_cursor_text.1, raw_cursor_text.2) - cursor_lum).abs() < 0.12 {
            if is_rgb_dark(cursor.0, cursor.1, cursor.2) {
                (0xFF, 0xFF, 0xFF)
            } else {
                (0x1C, 0x1C, 0x1E)
            }
        } else {
            raw_cursor_text
        };
        let selection = parse_hex_rgb(&self.selection).unwrap_or(if is_dark {
            (0x3B, 0x4E, 0x6B)
        } else {
            (0xB4, 0xD5, 0xFE)
        });
        let sel_lum = relative_luminance(selection.0, selection.1, selection.2);
        let raw_sel_text = parse_hex_rgb(&self.selection_text).unwrap_or(fg);
        let selection_text = if (relative_luminance(raw_sel_text.0, raw_sel_text.1, raw_sel_text.2) - sel_lum).abs() < 0.12 {
            if is_rgb_dark(selection.0, selection.1, selection.2) {
                (0xFF, 0xFF, 0xFF)
            } else {
                (0x1C, 0x1C, 0x1E)
            }
        } else {
            raw_sel_text
        };
        let raw_bold = parse_hex_rgb(&self.bold).unwrap_or(fg);
        let bold = if (relative_luminance(raw_bold.0, raw_bold.1, raw_bold.2) - bg_lum).abs() < 0.08 {
            fg
        } else {
            raw_bold
        };

        let fallback_ansi = if is_dark {
            &DEFAULT_ANSI16_DARK_HEX
        } else {
            &DEFAULT_ANSI16_LIGHT_HEX
        };
        let mut ansi = [(0u8, 0u8, 0u8); 16];
        for (i, hex_str) in self.ansi.iter().enumerate() {
            ansi[i] = parse_hex_rgb(hex_str)
                .or_else(|| parse_hex_rgb(fallback_ansi[i]))
                .unwrap_or(fg);
        }

        ResolvedTerminalPalette {
            explicit_scheme: true,
            is_dark,
            background: bg,
            foreground: fg,
            cursor,
            cursor_text,
            selection,
            selection_text,
            bold,
            ansi,
        }
    }
}

/// Pre-parsed RGB palette stored in each `TermBuffer` for fast, allocation-free
/// span color resolution.
///
/// When `explicit_scheme` is `false`, `presentation.rs` uses MeatShell's built-in
/// adaptive dark/light logic (preserving 100% backwards compatibility with
/// existing light-mode contrast remapping). When `explicit_scheme` is `true`,
/// `presentation.rs` renders the exact theme colors (foreground, background,
/// ANSI 0..15, and 24-bit True Color).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedTerminalPalette {
    pub explicit_scheme: bool,
    pub is_dark: bool,
    pub background: (u8, u8, u8),
    pub foreground: (u8, u8, u8),
    pub cursor: (u8, u8, u8),
    pub cursor_text: (u8, u8, u8),
    pub selection: (u8, u8, u8),
    pub selection_text: (u8, u8, u8),
    pub bold: (u8, u8, u8),
    pub ansi: [(u8, u8, u8); 16],
}

impl Default for ResolvedTerminalPalette {
    fn default() -> Self {
        Self::default_for_mode(true)
    }
}

impl ResolvedTerminalPalette {
    pub fn default_for_mode(is_dark: bool) -> Self {
        let mut ansi = [(0u8, 0u8, 0u8); 16];
        let table = if is_dark {
            &DEFAULT_ANSI16_DARK_HEX
        } else {
            &DEFAULT_ANSI16_LIGHT_HEX
        };
        for (i, h) in table.iter().enumerate() {
            ansi[i] = parse_hex_rgb(h).unwrap_or((0xD4, 0xD4, 0xD4));
        }
        if is_dark {
            Self {
                explicit_scheme: false,
                is_dark: true,
                background: (0x0E, 0x0F, 0x13),
                foreground: (0xD4, 0xD4, 0xD4),
                cursor: (0xD4, 0xD4, 0xD4),
                cursor_text: (0x0E, 0x0F, 0x13),
                selection: (0x3B, 0x82, 0xF6),
                selection_text: (0xFF, 0xFF, 0xFF),
                bold: (0xFF, 0xFF, 0xFF),
                ansi,
            }
        } else {
            Self {
                explicit_scheme: false,
                is_dark: false,
                background: (0xFA, 0xFA, 0xFA),
                foreground: (0x2D, 0x2D, 0x2F),
                cursor: (0x2D, 0x2D, 0x2F),
                cursor_text: (0xFA, 0xFA, 0xFA),
                selection: (0x25, 0x63, 0xEB),
                selection_text: (0xFF, 0xFF, 0xFF),
                bold: (0x1C, 0x1C, 0x1E),
                ansi,
            }
        }
    }
}

// ── Hex & Luminance Helpers ──────────────────────────────────────────────────

pub fn parse_hex_rgb(hex: &str) -> Option<(u8, u8, u8)> {
    let clean = hex.trim().strip_prefix('#').unwrap_or(hex.trim());
    if clean.len() != 6 || !clean.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&clean[0..2], 16).ok()?;
    let g = u8::from_str_radix(&clean[2..4], 16).ok()?;
    let b = u8::from_str_radix(&clean[4..6], 16).ok()?;
    Some((r, g, b))
}

pub fn rgb_to_hex((r, g, b): (u8, u8, u8)) -> String {
    format!("#{:02X}{:02X}{:02X}", r, g, b)
}

/// Relative luminance per WCAG 2.1. Returns `true` when background is dark (`L < 0.35`).
pub fn relative_luminance(r: u8, g: u8, b: u8) -> f32 {
    let channel = |c: u8| -> f32 {
        let s = c as f32 / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
}

pub fn is_rgb_dark(r: u8, g: u8, b: u8) -> bool {
    relative_luminance(r, g, b) < 0.35
}

pub fn slugify_theme_id(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_dash = false;
    for ch in name.trim().replace('+', " plus ").chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "custom-theme".to_string()
    } else {
        out
    }
}

// ── Native `.itermcolors` XML Plist Parser & Exporter ────────────────────────

/// Parse an iTerm2 `.itermcolors` XML Plist document into a [`TerminalTheme`].
///
/// Missing optional colors (such as `Cursor Color`, `Selection Color`, `Bold Color`,
/// or individual ANSI slots) are filled with safe, contrast-appropriate defaults
/// while preserving the raw parsed color map in `raw_colors`.
pub fn parse_itermcolors_xml(name: &str, xml: &str) -> Result<TerminalTheme> {
    let outer_dict = extract_outer_plist_dict(xml)
        .context("invalid .itermcolors plist: missing top-level <dict>")?;

    let mut raw_map: HashMap<String, String> = HashMap::new();
    let mut cursor = 0usize;

    while let Some((key, after_key)) = next_xml_tag_text(outer_dict, cursor, "key") {
        let key_trimmed = xml_unescape(key.trim());
        // Find the `<dict>...</dict>` immediately following this `<key>`
        let Some((dict_body, after_dict)) = next_xml_tag_text(outer_dict, after_key, "dict") else {
            break;
        };
        cursor = after_dict;
        if let Some(rgb) = parse_color_component_dict(dict_body) {
            raw_map.insert(key_trimmed, rgb_to_hex(rgb));
        }
    }

    if raw_map.is_empty() {
        anyhow::bail!("no valid color entries found in .itermcolors file");
    }

    let bg_hex = raw_map
        .get("Background Color")
        .or_else(|| raw_map.get("Ansi 0 Color"))
        .cloned()
        .unwrap_or_else(|| "#212121".to_string());
    let bg_rgb = parse_hex_rgb(&bg_hex).unwrap_or((0x21, 0x21, 0x21));
    let is_dark = is_rgb_dark(bg_rgb.0, bg_rgb.1, bg_rgb.2);

    let default_fg = if is_dark { "#D4D4D4" } else { "#2D2D2F" };
    let fg_hex = raw_map
        .get("Foreground Color")
        .or_else(|| raw_map.get("Ansi 7 Color"))
        .cloned()
        .unwrap_or_else(|| default_fg.to_string());

    let cursor_hex = raw_map
        .get("Cursor Color")
        .cloned()
        .unwrap_or_else(|| fg_hex.clone());
    let cursor_text_hex = raw_map
        .get("Cursor Text Color")
        .cloned()
        .unwrap_or_else(|| bg_hex.clone());

    let default_sel = if is_dark { "#3B4E6B" } else { "#B4D5FE" };
    let selection_hex = raw_map
        .get("Selection Color")
        .cloned()
        .unwrap_or_else(|| default_sel.to_string());
    let selection_text_hex = raw_map
        .get("Selected Text Color")
        .cloned()
        .unwrap_or_else(|| fg_hex.clone());
    let bold_hex = raw_map
        .get("Bold Color")
        .cloned()
        .unwrap_or_else(|| fg_hex.clone());

    let fallback_ansi = if is_dark {
        &DEFAULT_ANSI16_DARK_HEX
    } else {
        &DEFAULT_ANSI16_LIGHT_HEX
    };
    let mut ansi: [String; 16] = std::array::from_fn(|i| fallback_ansi[i].to_string());
    for i in 0..16 {
        let key = format!("Ansi {i} Color");
        if let Some(val) = raw_map.get(&key) {
            ansi[i] = val.clone();
        } else if i >= 8 {
            // If bright slot is missing, fall back to corresponding normal slot (i - 8) if present
            let normal_key = format!("Ansi {} Color", i - 8);
            if let Some(normal_val) = raw_map.get(&normal_key) {
                ansi[i] = normal_val.clone();
            }
        }
    }

    let clean_name = if name.trim().is_empty() {
        "Imported iTerm2 Theme"
    } else {
        name.trim()
    };

    Ok(TerminalTheme {
        theme_id: slugify_theme_id(clean_name),
        name: clean_name.to_string(),
        author: "Custom / iTerm2".to_string(),
        license: ITERM2_LICENSE.to_string(),
        source: format!("{clean_name}.itermcolors"),
        is_dark,
        background: bg_hex,
        foreground: fg_hex,
        cursor: cursor_hex,
        cursor_text: cursor_text_hex,
        selection: selection_hex,
        selection_text: selection_text_hex,
        bold: bold_hex,
        ansi,
        raw_colors: Some(raw_map),
    }
    .normalized())
}

fn extract_outer_plist_dict(xml: &str) -> Option<&str> {
    let start_tag = "<dict>";
    let end_tag = "</dict>";
    let start = xml.find(start_tag)? + start_tag.len();
    let end = xml.rfind(end_tag)?;
    (start <= end).then_some(&xml[start..end])
}

fn next_xml_tag_text<'a>(s: &'a str, from: usize, tag: &str) -> Option<(&'a str, usize)> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let rel_start = s.get(from..)?.find(&open)?;
    let content_start = from + rel_start + open.len();
    let rel_end = s.get(content_start..)?.find(&close)?;
    let content_end = content_start + rel_end;
    let after = content_end + close.len();
    Some((&s[content_start..content_end], after))
}

fn parse_color_component_dict(dict_body: &str) -> Option<(u8, u8, u8)> {
    let mut r: Option<f64> = None;
    let mut g: Option<f64> = None;
    let mut b: Option<f64> = None;
    let mut cursor = 0usize;

    while let Some((k, after_k)) = next_xml_tag_text(dict_body, cursor, "key") {
        let key_name = k.trim();
        // Find next value tag after `after_k`
        let rest = dict_body.get(after_k..)?;
        let next_key_pos = rest.find("<key>").unwrap_or(rest.len());
        let value_slice = &rest[..next_key_pos];
        cursor = after_k + next_key_pos;

        let number = next_xml_tag_text(value_slice, 0, "real")
            .or_else(|| next_xml_tag_text(value_slice, 0, "integer"))
            .and_then(|(v, _)| v.trim().parse::<f64>().ok());

        if let Some(mut val) = number {
            if val > 1.0 && val <= 255.0 {
                val /= 255.0;
            }
            let clamped = val.clamp(0.0, 1.0);
            match key_name {
                "Red Component" => r = Some(clamped),
                "Green Component" => g = Some(clamped),
                "Blue Component" => b = Some(clamped),
                _ => {}
            }
        }
    }

    let (r, g, b) = (r?, g?, b?);
    Some((
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
    ))
}

fn xml_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Export a [`TerminalTheme`] back into standard `.itermcolors` XML Plist format.
pub fn export_itermcolors_xml(theme: &TerminalTheme) -> String {
    let mut entries: Vec<(String, String)> = Vec::with_capacity(23);
    for i in 0..16 {
        entries.push((format!("Ansi {i} Color"), theme.ansi[i].clone()));
    }
    entries.push(("Background Color".to_string(), theme.background.clone()));
    entries.push(("Bold Color".to_string(), theme.bold.clone()));
    entries.push(("Cursor Color".to_string(), theme.cursor.clone()));
    entries.push(("Cursor Text Color".to_string(), theme.cursor_text.clone()));
    entries.push(("Foreground Color".to_string(), theme.foreground.clone()));
    entries.push((
        "Selected Text Color".to_string(),
        theme.selection_text.clone(),
    ));
    entries.push(("Selection Color".to_string(), theme.selection.clone()));

    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n",
    );
    for (key, hex) in entries {
        let (r, g, b) = parse_hex_rgb(&hex).unwrap_or((0, 0, 0));
        let rf = r as f64 / 255.0;
        let gf = g as f64 / 255.0;
        let bf = b as f64 / 255.0;
        xml.push_str(&format!(
            "\t<key>{}</key>\n\
             \t<dict>\n\
             \t\t<key>Color Space</key>\n\
             \t\t<string>sRGB</string>\n\
             \t\t<key>Blue Component</key>\n\
             \t\t<real>{bf:.8}</real>\n\
             \t\t<key>Green Component</key>\n\
             \t\t<real>{gf:.8}</real>\n\
             \t\t<key>Red Component</key>\n\
             \t\t<real>{rf:.8}</real>\n\
             \t</dict>\n",
            xml_escape(&key)
        ));
    }
    xml.push_str("</dict>\n</plist>\n");
    xml
}

// ── Flexible JSON Theme Parser & Exporter ────────────────────────────────────

/// Intermediate flexible JSON representation so users can import JSON files
/// using either `"ansi": ["#...", ...]` OR `"ansi_palette": { "normal": {...}, "bright": {...} }`,
/// and with optional fields safely defaulted.
#[derive(Deserialize)]
struct FlexibleThemeJson {
    #[serde(default)]
    theme_id: String,
    name: String,
    #[serde(default)]
    author: String,
    #[serde(default)]
    license: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    background: String,
    #[serde(default)]
    foreground: String,
    #[serde(default)]
    cursor: String,
    #[serde(default)]
    cursor_text: String,
    #[serde(default)]
    selection: String,
    #[serde(default)]
    selection_text: String,
    #[serde(default)]
    bold: String,
    #[serde(default)]
    ansi: Option<Vec<String>>,
    #[serde(default)]
    ansi_palette: Option<AnsiPaletteNamed>,
}

/// Parse a single [`TerminalTheme`] from JSON text.
pub fn parse_theme_json(json_str: &str) -> Result<TerminalTheme> {
    let flex: FlexibleThemeJson =
        serde_json::from_str(json_str).context("invalid terminal theme JSON")?;
    if flex.name.trim().is_empty() {
        anyhow::bail!("theme JSON must include a non-empty 'name'");
    }
    let bg_rgb = parse_hex_rgb(&flex.background).unwrap_or((0x21, 0x21, 0x21));
    let is_dark = is_rgb_dark(bg_rgb.0, bg_rgb.1, bg_rgb.2);
    let fallback_ansi = if is_dark {
        &DEFAULT_ANSI16_DARK_HEX
    } else {
        &DEFAULT_ANSI16_LIGHT_HEX
    };
    let mut ansi: [String; 16] = std::array::from_fn(|i| fallback_ansi[i].to_string());
    if let Some(named) = flex.ansi_palette {
        ansi = named.to_array();
    } else if let Some(list) = flex.ansi {
        for (i, item) in list.into_iter().take(16).enumerate() {
            ansi[i] = item;
        }
    }

    Ok(TerminalTheme {
        theme_id: if flex.theme_id.trim().is_empty() {
            slugify_theme_id(&flex.name)
        } else {
            flex.theme_id
        },
        name: flex.name,
        author: flex.author,
        license: flex.license,
        source: if flex.source.trim().is_empty() {
            "custom-json".to_string()
        } else {
            flex.source
        },
        is_dark,
        background: flex.background,
        foreground: flex.foreground,
        cursor: flex.cursor,
        cursor_text: flex.cursor_text,
        selection: flex.selection,
        selection_text: flex.selection_text,
        bold: flex.bold,
        ansi,
        raw_colors: None,
    }
    .normalized())
}

/// Export a [`TerminalTheme`] as a formatted JSON document including both the
/// 16-element `ansi` array and the structured `ansi_palette` object.
pub fn export_theme_json(theme: &TerminalTheme) -> Result<String> {
    #[derive(Serialize)]
    struct ExportedTheme<'a> {
        theme_id: &'a str,
        name: &'a str,
        author: &'a str,
        license: &'a str,
        source: &'a str,
        is_dark: bool,
        background: &'a str,
        foreground: &'a str,
        cursor: &'a str,
        cursor_text: &'a str,
        selection: &'a str,
        selection_text: &'a str,
        bold: &'a str,
        ansi: &'a [String; 16],
        ansi_palette: AnsiPaletteNamed,
    }

    let payload = ExportedTheme {
        theme_id: &theme.theme_id,
        name: &theme.name,
        author: &theme.author,
        license: &theme.license,
        source: &theme.source,
        is_dark: theme.is_dark,
        background: &theme.background,
        foreground: &theme.foreground,
        cursor: &theme.cursor,
        cursor_text: &theme.cursor_text,
        selection: &theme.selection,
        selection_text: &theme.selection_text,
        bold: &theme.bold,
        ansi: &theme.ansi,
        ansi_palette: theme.ansi_palette_named(),
    };
    serde_json::to_string_pretty(&payload).context("failed to serialize theme JSON")
}

// ── Built-in iTerm2-Color-Schemes Catalog ────────────────────────────────────

static BUILTIN_THEMES: OnceLock<Vec<TerminalTheme>> = OnceLock::new();

const BUNDLED_ITERM2_JSON: &str = include_str!("../assets/iterm2_themes.json");

/// Return all built-in `iTerm2-Color-Schemes` themes (915 community themes).
/// If parsing ever failed, returns a safe fallback slice so the terminal never crashes.
pub fn builtin_iterm2_themes() -> &'static [TerminalTheme] {
    BUILTIN_THEMES.get_or_init(|| {
        match serde_json::from_str::<Vec<TerminalTheme>>(BUNDLED_ITERM2_JSON) {
            Ok(list) if !list.is_empty() => list,
            Ok(_) | Err(_) => vec![TerminalTheme::default_for_mode(true)],
        }
    })
}

// ── ThemeManager ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeCategory {
    All,
    Dark,
    Light,
    Favorites,
    Custom,
}

impl ThemeCategory {
    pub fn from_id(id: &str) -> Self {
        match id.trim().to_ascii_lowercase().as_str() {
            "dark" => Self::Dark,
            "light" => Self::Light,
            "favorites" | "fav" | "starred" => Self::Favorites,
            "custom" => Self::Custom,
            _ => Self::All,
        }
    }
}

/// Independent manager for built-in iTerm2 color schemes, user favorites,
/// custom themes, search/filtering, runtime switching, and import/export.
#[derive(Debug, Clone)]
pub struct ThemeManager {
    current_theme_id: String,
    favorites: Vec<String>,
    custom_themes: Vec<TerminalTheme>,
    workbench_is_dark: bool,
}

impl Default for ThemeManager {
    fn default() -> Self {
        Self::new(DEFAULT_THEME_ID.to_string(), Vec::new(), Vec::new(), true)
    }
}

impl ThemeManager {
    pub fn new(
        current_theme_id: String,
        favorites: Vec<String>,
        custom_themes: Vec<TerminalTheme>,
        workbench_is_dark: bool,
    ) -> Self {
        let normalized_id = if current_theme_id.trim().is_empty() {
            DEFAULT_THEME_ID.to_string()
        } else {
            current_theme_id.trim().to_string()
        };
        Self {
            current_theme_id: normalized_id,
            favorites,
            custom_themes: custom_themes
                .into_iter()
                .map(TerminalTheme::normalized)
                .collect(),
            workbench_is_dark,
        }
    }

    pub fn custom_themes(&self) -> &[TerminalTheme] {
        &self.custom_themes
    }

    pub fn is_favorite(&self, theme_id: &str) -> bool {
        self.favorites.iter().any(|id| id == theme_id)
    }

    pub fn is_custom(&self, theme_id: &str) -> bool {
        self.custom_themes.iter().any(|t| t.theme_id == theme_id)
    }

    /// Toggle favorite status for `theme_id`. Returns the new favorite state (`true` = favorited).
    #[cfg(test)]
    pub fn toggle_favorite(&mut self, theme_id: &str) -> bool {
        if let Some(pos) = self.favorites.iter().position(|id| id == theme_id) {
            self.favorites.remove(pos);
            false
        } else {
            self.favorites.push(theme_id.to_string());
            true
        }
    }

    /// Look up any theme by ID (`"default"`, custom theme, or built-in iTerm2 theme).
    pub fn get_theme(&self, theme_id: &str) -> Option<TerminalTheme> {
        let id = if theme_id.trim().is_empty() {
            DEFAULT_THEME_ID
        } else {
            theme_id.trim()
        };
        if id == DEFAULT_THEME_ID {
            return Some(TerminalTheme::default_for_mode(self.workbench_is_dark));
        }
        if let Some(custom) = self.custom_themes.iter().find(|t| t.theme_id == id) {
            return Some(custom.clone());
        }
        builtin_iterm2_themes()
            .iter()
            .find(|t| t.theme_id.eq_ignore_ascii_case(id) || t.name.eq_ignore_ascii_case(id))
            .cloned()
    }

    /// Return the active [`TerminalTheme`], falling back safely to the default theme
    /// if the saved `current_theme_id` does not exist.
    pub fn current_theme(&self) -> TerminalTheme {
        self.get_theme(&self.current_theme_id)
            .unwrap_or_else(|| TerminalTheme::default_for_mode(self.workbench_is_dark))
    }

    /// Switch the active theme by ID. If `theme_id` is not found, falls back to `"default"`.
    #[cfg(test)]
    pub fn set_current_theme(&mut self, theme_id: &str) -> TerminalTheme {
        if let Some(theme) = self.get_theme(theme_id) {
            self.current_theme_id = theme.theme_id.clone();
            theme
        } else {
            self.current_theme_id = DEFAULT_THEME_ID.to_string();
            TerminalTheme::default_for_mode(self.workbench_is_dark)
        }
    }

    /// Return all available themes: the adaptive default first, then user custom themes,
    /// followed by all 915 built-in `iTerm2-Color-Schemes`.
    #[cfg(test)]
    pub fn all_themes(&self) -> Vec<TerminalTheme> {
        let builtins = builtin_iterm2_themes();
        let mut out = Vec::with_capacity(1 + self.custom_themes.len() + builtins.len());
        out.push(TerminalTheme::default_for_mode(self.workbench_is_dark));
        out.extend(self.custom_themes.iter().cloned());
        out.extend(builtins.iter().cloned());
        out
    }

    /// Search themes by name, theme_id, or author (case-insensitive substring match).
    #[cfg(test)]
    pub fn search(&self, query: &str) -> Vec<TerminalTheme> {
        self.filter(query, ThemeCategory::All)
    }

    /// Filter themes by category (`All`, `Dark`, `Light`, `Favorites`, `Custom`) and search query.
    ///
    /// Ordering:
    /// 1. The `"default"` theme (when matching)
    /// 2. Favorited themes first
    /// 3. Exact/prefix matches on query, then alphabetical by name
    pub fn filter(&self, query: &str, category: ThemeCategory) -> Vec<TerminalTheme> {
        let q = query.trim().to_ascii_lowercase();
        let default_theme = TerminalTheme::default_for_mode(self.workbench_is_dark);
        let builtins = builtin_iterm2_themes();
        let mut refs: Vec<&TerminalTheme> = std::iter::once(&default_theme)
            .chain(self.custom_themes.iter())
            .chain(builtins.iter())
            .filter(|t| match category {
                ThemeCategory::All => true,
                ThemeCategory::Dark => t.is_dark,
                ThemeCategory::Light => !t.is_dark,
                ThemeCategory::Favorites => self.is_favorite(&t.theme_id),
                ThemeCategory::Custom => self.is_custom(&t.theme_id),
            })
            .filter(|t| {
                if q.is_empty() {
                    return true;
                }
                t.name.to_ascii_lowercase().contains(&q)
                    || t.theme_id.to_ascii_lowercase().contains(&q)
                    || t.author.to_ascii_lowercase().contains(&q)
            })
            .collect();

        refs.sort_by(|a, b| {
            let a_def = a.theme_id == DEFAULT_THEME_ID;
            let b_def = b.theme_id == DEFAULT_THEME_ID;
            if a_def != b_def {
                return b_def.cmp(&a_def);
            }
            let a_fav = self.is_favorite(&a.theme_id);
            let b_fav = self.is_favorite(&b.theme_id);
            if a_fav != b_fav {
                return b_fav.cmp(&a_fav);
            }
            if !q.is_empty() {
                let a_starts = a.name.to_ascii_lowercase().starts_with(&q);
                let b_starts = b.name.to_ascii_lowercase().starts_with(&q);
                if a_starts != b_starts {
                    return b_starts.cmp(&a_starts);
                }
            }
            a.name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase())
        });

        refs.into_iter().cloned().collect()
    }

    /// Add or update a user custom theme. Avoids colliding with `"default"` or built-in IDs.
    pub fn upsert_custom_theme(&mut self, mut theme: TerminalTheme) -> TerminalTheme {
        theme = theme.normalized();
        if theme.theme_id == DEFAULT_THEME_ID
            || builtin_iterm2_themes()
                .iter()
                .any(|b| b.theme_id == theme.theme_id)
        {
            theme.theme_id = format!("custom-{}", theme.theme_id);
        }
        if let Some(existing) = self
            .custom_themes
            .iter_mut()
            .find(|t| t.theme_id == theme.theme_id)
        {
            *existing = theme.clone();
        } else {
            self.custom_themes.push(theme.clone());
        }
        theme
    }

    /// Import a theme from a `.itermcolors` or `.json` file on disk and register it
    /// as a custom theme.
    pub fn import_theme_from_path(&mut self, path: &Path) -> Result<TerminalTheme> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read theme file {}", path.display()))?;
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Imported Theme");
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        let parsed = if ext == "itermcolors" || raw.trim_start().starts_with("<?xml") || raw.contains("<plist") {
            parse_itermcolors_xml(stem, &raw)?
        } else {
            parse_theme_json(&raw)?
        };

        Ok(self.upsert_custom_theme(parsed))
    }

    /// Export a theme by ID to a `.itermcolors` or `.json` file on disk.
    pub fn export_theme_to_path(&self, theme_id: &str, path: &Path) -> Result<()> {
        let theme = self
            .get_theme(theme_id)
            .with_context(|| format!("theme '{theme_id}' not found"))?;
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let content = if ext == "itermcolors" {
            export_itermcolors_xml(&theme)
        } else {
            export_theme_json(&theme)?
        };
        fs::write(path, content)
            .with_context(|| format!("failed to write theme file {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_all_915_builtin_iterm2_themes_with_dark_and_light_categories() {
        let themes = builtin_iterm2_themes();
        assert!(
            themes.len() >= 900,
            "expected at least 900 built-in iTerm2 themes, got {}",
            themes.len()
        );
        let dark_count = themes.iter().filter(|t| t.is_dark).count();
        let light_count = themes.iter().filter(|t| !t.is_dark).count();
        assert!(dark_count > 600, "expected >600 dark themes, got {dark_count}");
        assert!(light_count > 150, "expected >150 light themes, got {light_count}");
        assert_eq!(dark_count + light_count, themes.len());
    }

    #[test]
    fn verifies_canonical_ansi_16_color_mapping_for_dracula_and_solarized() {
        let mgr = ThemeManager::default();
        let dracula = mgr.get_theme("dracula").expect("Dracula must exist");
        assert!(dracula.is_dark);
        assert_eq!(dracula.background, "#282A36");
        assert_eq!(dracula.foreground, "#F8F8F2");
        let named = dracula.ansi_palette_named();
        // Verify ANSI slots 0..7 are in strict order (black, red, green, yellow, blue, magenta, cyan, white)
        assert_eq!(named.normal.black, dracula.ansi[0]);
        assert_eq!(named.normal.red, dracula.ansi[1]);
        assert_eq!(named.normal.green, dracula.ansi[2]);
        assert_eq!(named.normal.yellow, dracula.ansi[3]);
        assert_eq!(named.normal.blue, dracula.ansi[4]);
        assert_eq!(named.normal.magenta, dracula.ansi[5]);
        assert_eq!(named.normal.cyan, dracula.ansi[6]);
        assert_eq!(named.normal.white, dracula.ansi[7]);
        // Ensure Red slot (1) has dominant red channel and Green slot (2) has dominant green channel
        let (r1, g1, _b1) = parse_hex_rgb(&named.normal.red).unwrap();
        assert!(r1 > g1, "ANSI 1 (red) must be red-dominant: {}", named.normal.red);
        let (r2, g2, _b2) = parse_hex_rgb(&named.normal.green).unwrap();
        assert!(g2 > r2, "ANSI 2 (green) must be green-dominant: {}", named.normal.green);

        let solarized_light = mgr
            .get_theme("iterm2-solarized-light")
            .expect("Solarized Light must exist");
        assert!(!solarized_light.is_dark);
    }

    #[test]
    fn parses_itermcolors_xml_and_roundtrips_cleanly() {
        let original = ThemeManager::default()
            .get_theme("dracula")
            .expect("Dracula theme");
        let xml = export_itermcolors_xml(&original);
        let parsed = parse_itermcolors_xml("Dracula Roundtrip", &xml).expect("parse exported xml");
        assert_eq!(parsed.background, original.background);
        assert_eq!(parsed.foreground, original.foreground);
        assert_eq!(parsed.cursor, original.cursor);
        assert_eq!(parsed.selection, original.selection);
        assert_eq!(parsed.ansi, original.ansi);
        assert_eq!(parsed.is_dark, original.is_dark);
    }

    #[test]
    fn fills_missing_optional_colors_in_partial_itermcolors() {
        let partial_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Background Color</key>
    <dict>
        <key>Red Component</key><real>0.1</real>
        <key>Green Component</key><real>0.1</real>
        <key>Blue Component</key><real>0.12</real>
    </dict>
    <key>Foreground Color</key>
    <dict>
        <key>Red Component</key><real>0.9</real>
        <key>Green Component</key><real>0.9</real>
        <key>Blue Component</key><real>0.9</real>
    </dict>
    <key>Ansi 1 Color</key>
    <dict>
        <key>Red Component</key><real>1.0</real>
        <key>Green Component</key><real>0.2</real>
        <key>Blue Component</key><real>0.2</real>
    </dict>
</dict>
</plist>"#;
        let theme = parse_itermcolors_xml("Partial Dark", partial_xml).unwrap();
        assert!(theme.is_dark);
        assert_eq!(theme.background, "#1A1A1F");
        assert_eq!(theme.foreground, "#E6E6E6");
        // Cursor defaults to foreground when omitted
        assert_eq!(theme.cursor, "#E6E6E6");
        // Ansi 1 was provided
        assert_eq!(theme.ansi[1], "#FF3333");
        // Ansi 9 (bright red) inherits Ansi 1 when omitted
        assert_eq!(theme.ansi[9], "#FF3333");
        // Ansi 2 (green) falls back to safe dark palette green
        assert_eq!(theme.ansi[2], DEFAULT_ANSI16_DARK_HEX[2]);
        // Raw colors preserved
        assert_eq!(theme.raw_colors.as_ref().unwrap().len(), 3);
    }

    #[test]
    fn theme_manager_search_filter_favorites_and_custom_crud() {
        let mut mgr = ThemeManager::default();
        let catppuccin = mgr.search("catppuccin");
        assert!(!catppuccin.is_empty());

        // Toggle favorite
        let target_id = catppuccin[0].theme_id.clone();
        assert!(!mgr.is_favorite(&target_id));
        assert!(mgr.toggle_favorite(&target_id));
        assert!(mgr.is_favorite(&target_id));

        let favs = mgr.filter("", ThemeCategory::Favorites);
        assert_eq!(favs.len(), 1);
        assert_eq!(favs[0].theme_id, target_id);

        // Add custom theme
        let mut custom = TerminalTheme::default_for_mode(true);
        custom.theme_id = "my-neon-shell".to_string();
        custom.name = "My Neon Shell".to_string();
        let added = mgr.upsert_custom_theme(custom);
        assert_eq!(added.theme_id, "my-neon-shell");

        let customs = mgr.filter("", ThemeCategory::Custom);
        assert_eq!(customs.len(), 1);
        assert_eq!(customs[0].name, "My Neon Shell");

        // Switch & fallback on unknown id
        let switched = mgr.set_current_theme("my-neon-shell");
        assert_eq!(switched.theme_id, "my-neon-shell");
        let fallback = mgr.set_current_theme("non-existent-theme-xyz");
        assert_eq!(fallback.theme_id, DEFAULT_THEME_ID);
    }
}
