//! UI and terminal font discovery.

use super::*;

/// Enumerate installed monospace font families for the Interface font picker.
/// Terminals want fixed-width fonts, so non-monospace families are filtered out.
/// Choose a UI font family that fontdb can actually resolve, falling back to the
/// embedded "Meatshell Mono" when the system font database is empty/unreadable.
///
/// macOS 26 (Tahoe) shipped a system where fontdb couldn't register the named
/// CJK font ("PingFang SC"), so hard-coding that name made the whole UI render
/// blank (#129). This probes the loaded faces and picks the first CJK-capable
/// family that exists; if none do, it returns the embedded font so the window is
/// still visible (Latin text shows; CJK may tofu — far better than a blank UI).
///
/// Emits a one-line WARN summary (faces loaded + chosen font) so the choice lands
/// in `error.log` for diagnostics without needing RUST_LOG.
pub(super) fn resolve_ui_font_family() -> slint::SharedString {
    use fontdb::{Database, Family, Query, Stretch, Style, Weight};

    // Diagnostic / escape hatch (#129): force a specific UI font without a rebuild.
    // e.g. MEATSHELL_UI_FONT="Meatshell Mono" to test whether the embedded font
    // renders when system fonts don't. Empty value is ignored.
    if let Some(f) = std::env::var_os("MEATSHELL_UI_FONT") {
        let f = f.to_string_lossy().into_owned();
        if !f.trim().is_empty() {
            tracing::debug!(font = %f, "ui-font: overridden via MEATSHELL_UI_FONT");
            return f.into();
        }
    }

    let mut db = Database::new();
    db.load_system_fonts();
    let face_count = db.faces().count();

    // CJK-capable system families, most-preferred first, per platform. The UI
    // default font must cover CJK because TextInput doesn't glyph-fallback (#54).
    //
    // macOS note (#129): the modern system CJK fonts (PingFang SC, Hiragino) fail
    // to rasterize under femtovg on some macOS 26 machines — fontdb finds them but
    // every glyph comes out blank. The older Heiti/Songti faces render fine and
    // ship on every macOS, so we prefer them and keep PingFang only as a late
    // fallback. (Verified on an M2/macOS 26: Heiti SC/STHeiti/Songti SC render,
    // PingFang/Hiragino don't.) Power users can still force one via
    // MEATSHELL_UI_FONT. Heiti SC is a clean sans-serif (better for UI than the
    // serif Songti), so it leads.
    #[cfg(target_os = "macos")]
    let candidates: &[&str] = &[
        "Heiti SC",
        "STHeiti",
        "Songti SC",
        "PingFang SC",
        "Hiragino Sans GB",
    ];
    #[cfg(target_os = "windows")]
    let candidates: &[&str] = &["Microsoft YaHei UI", "Microsoft YaHei", "SimHei", "SimSun"];
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let candidates: &[&str] = &[
        "Noto Sans CJK SC",
        "Noto Sans CJK",
        "Source Han Sans SC",
        "WenQuanYi Micro Hei",
        "Droid Sans Fallback",
    ];

    for name in candidates {
        let q = Query {
            families: &[Family::Name(name)],
            weight: Weight::NORMAL,
            stretch: Stretch::Normal,
            style: Style::Normal,
        };
        if db.query(&q).is_some() {
            tracing::debug!(
                faces = face_count,
                font = name,
                "ui-font: using system CJK font"
            );
            return (*name).into();
        }
    }

    // No preferred family resolved. List what *is* available (if anything) so the
    // log shows whether enumeration is empty or just missing our candidates (#129).
    if face_count > 0 {
        let mut fams: Vec<String> = db
            .faces()
            .filter_map(|f| f.families.first().map(|(n, _)| n.clone()))
            .collect();
        fams.sort();
        fams.dedup();
        let sample: Vec<String> = fams.into_iter().take(40).collect();
        tracing::warn!(faces = face_count, available = ?sample,
            "ui-font: no preferred CJK font resolved; listing available families");
    }
    tracing::warn!(
        faces = face_count,
        "ui-font: falling back to embedded 'Meatshell Mono' (system fonts unusable, #129)"
    );
    "Meatshell Mono".into()
}

pub(super) fn system_monospace_fonts() -> Vec<slint::SharedString> {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let mut names: Vec<String> = db
        .faces()
        .filter(|f| f.monospaced)
        .filter_map(|f| f.families.first().map(|(n, _)| n.clone()))
        .collect();
    names.sort();
    names.dedup();
    // Surface the built-in glyph-complete font first so it's selectable and the
    // default selection is shown — it isn't a system face so fontdb won't list it
    // (#114).
    names.retain(|n| n != "Meatshell Mono");
    let mut out = vec![slint::SharedString::from("Meatshell Mono")];
    out.extend(names.into_iter().map(slint::SharedString::from));
    out
}

/// System families tried, in order, for glyphs the terminal font lacks (#461).
/// Symbol-heavy fonts first; a Nerd Font symbols-only face wins when present
/// so Powerline/devicon glyphs in prompts render too.
#[cfg(target_os = "windows")]
const GLYPH_FALLBACK_FAMILIES: &[&str] = &[
    "Symbols Nerd Font Mono",
    "Segoe UI Symbol",
    "Cambria Math",
    "Segoe UI",
    "Microsoft YaHei",
];
#[cfg(target_os = "macos")]
const GLYPH_FALLBACK_FAMILIES: &[&str] = &[
    "Symbols Nerd Font Mono",
    "Menlo",
    "Apple Symbols",
    "Zapf Dingbats",
    "STIX Two Math",
];
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const GLYPH_FALLBACK_FAMILIES: &[&str] = &[
    "Symbols Nerd Font Mono",
    "DejaVu Sans Mono",
    "Noto Sans Mono",
    "DejaVu Sans",
    "Noto Sans Symbols 2",
    "Noto Sans Symbols",
    "Noto Sans Math",
    "Symbola",
];

fn face_coverage(db: &fontdb::Database, family: &str) -> Option<crate::terminal::Coverage> {
    use fontdb::{Family, Query, Stretch, Style, Weight};
    let id = db.query(&Query {
        families: &[Family::Name(family)],
        weight: Weight::NORMAL,
        stretch: Stretch::Normal,
        style: Style::Normal,
    })?;
    db.with_face_data(id, |data, index| {
        crate::terminal::Coverage::from_font_data(data, index)
    })
    .flatten()
}

/// Build the glyph-fallback table for `term_family` (empty = bundled font).
/// `None` when the terminal font can't be read (we can't tell what it lacks)
/// or no fallback family is installed; rendering then stays as before.
fn build_glyph_fallback(term_family: &str) -> Option<crate::terminal::GlyphFallback> {
    use crate::terminal::{Coverage, GlyphFallback, BUNDLED_TERM_FONT};
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let primary = if term_family == BUNDLED_TERM_FONT {
        Coverage::bundled()
    } else {
        face_coverage(&db, term_family)?
    };
    let fallbacks: Vec<_> = GLYPH_FALLBACK_FAMILIES
        .iter()
        .filter(|family| !family.eq_ignore_ascii_case(term_family))
        .filter_map(|family| {
            face_coverage(&db, family)
                .map(|coverage| (slint::SharedString::from(*family), coverage))
        })
        .collect();
    tracing::debug!(
        term_font = term_family,
        fallbacks = ?fallbacks.iter().map(|(family, _)| family.as_str()).collect::<Vec<_>>(),
        "terminal glyph fallback resolved"
    );
    (!fallbacks.is_empty()).then(|| GlyphFallback::new(primary, fallbacks))
}

/// (Re)build the terminal glyph-fallback table for the current terminal font on
/// a background thread (font enumeration and cmap parsing stay off the UI
/// thread), then redraw this window's terminals so already-printed prompts pick
/// up the fallback. Repeated calls for the same family are no-ops (#461).
pub(super) fn refresh_glyph_fallback(term_family: &str, window: &AppWindow, bufs: &TermBuffers) {
    use std::sync::Mutex;
    static CURRENT: Mutex<Option<String>> = Mutex::new(None);

    let family = if term_family.trim().is_empty() {
        crate::terminal::BUNDLED_TERM_FONT.to_string()
    } else {
        term_family.to_string()
    };
    {
        let mut current = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
        if current.as_deref() == Some(family.as_str()) {
            return;
        }
        *current = Some(family.clone());
    }

    let weak = window.as_weak();
    let bufs = bufs.clone();
    let spawned = std::thread::Builder::new()
        .name("glyph-fallback".into())
        .spawn(move || {
            let fallback = build_glyph_fallback(&family);
            // A newer font choice may have superseded this one meanwhile.
            if CURRENT.lock().unwrap_or_else(|e| e.into_inner()).as_deref() != Some(family.as_str())
            {
                return;
            }
            crate::terminal::install_glyph_fallback(fallback);
            let _ = weak.upgrade_in_event_loop(move |window| {
                let tab_ids: Vec<String> = bufs.lock().unwrap().keys().cloned().collect();
                for tab_id in tab_ids {
                    rebuild_tab_display(&window, &bufs, &tab_id);
                }
            });
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "failed to start glyph-fallback thread");
    }
}
