//! Settings › Terminal colours: the scheme picker, `.itermcolors` import, and
//! applying the chosen dark / light pair (see `crate::terminal::scheme`).
//!
//! The pair is process-wide: rendering reads it from `terminal::scheme`, and
//! each window's Theme gets the background / text / cursor / selection colours.

use super::*;
use crate::terminal::scheme::{self, Rgb, SchemeColors, SchemeSource, TermScheme};

fn color((r, g, b): Rgb) -> slint::Color {
    slint::Color::from_rgb_u8(r, g, b)
}

/// The colours for a saved id: the scheme for rendering (`None` = default)
/// and the Theme's terminal colours.
fn resolve(dark: bool, id: &str) -> (Option<SchemeColors>, TermColors) {
    match scheme::lookup(id) {
        Some(c) => {
            let theme = TermColors {
                custom: true,
                bg: color(c.bg),
                fg: color(c.fg),
                cursor: color(c.cursor),
                selection: color(scheme::readable_selection(&c)),
            };
            (Some(c), theme)
        }
        None => {
            let d = scheme::default_colors(dark);
            let theme = TermColors {
                custom: false,
                bg: color(d.bg),
                fg: color(d.fg),
                cursor: color(d.cursor),
                selection: color(d.selection),
            };
            (None, theme)
        }
    }
}

/// The saved id, or "default" when it no longer resolves (deleted import).
fn current_id(store: &ConfigStore, dark: bool) -> String {
    let id = store.terminal_scheme(dark);
    if scheme::lookup(id).is_some() {
        id.to_string()
    } else {
        scheme::DEFAULT_ID.to_string()
    }
}

fn item(s: &TermScheme) -> TermSchemeItem {
    let en = crate::i18n::is_en();
    let (name, subtitle) = match s.source {
        SchemeSource::Default => (
            if en { "Default" } else { "默认" }.to_string(),
            if en {
                "MeatShell · follows the color theme"
            } else {
                "MeatShell · 跟随配色主题"
            }
            .to_string(),
        ),
        SchemeSource::Builtin => (s.name.clone(), "iTerm2-Color-Schemes".to_string()),
        SchemeSource::Imported => (
            s.name.clone(),
            if en {
                "Imported .itermcolors"
            } else {
                "导入的 .itermcolors"
            }
            .to_string(),
        ),
    };
    TermSchemeItem {
        id: s.id.as_str().into(),
        name: name.into(),
        subtitle: subtitle.into(),
        dark: s.dark,
        imported: s.source == SchemeSource::Imported,
        bg: color(s.colors.bg),
        fg: color(s.colors.fg),
        cursor: color(s.colors.cursor),
        selection: color(match s.source {
            SchemeSource::Default => s.colors.selection,
            _ => scheme::readable_selection(&s.colors),
        }),
        ansi: ModelRc::new(VecModel::from(
            s.colors.ansi.iter().copied().map(color).collect::<Vec<_>>(),
        )),
    }
}

/// Rows for the slot being edited, filtered by the search text: default
/// first, then schemes made for that mode, then the rest.
fn rows(dark_slot: bool, query: &str) -> Vec<TermScheme> {
    let query = query.trim().to_lowercase();
    let mut rows: Vec<TermScheme> = scheme::all(dark_slot)
        .into_iter()
        .filter(|s| {
            query.is_empty()
                || s.name.to_lowercase().contains(&query)
                || s.id.to_lowercase().contains(&query)
                || (s.source == SchemeSource::Default
                    && ["default", "默认", "meatshell"]
                        .iter()
                        .any(|w| w.contains(&query)))
        })
        .collect();
    rows.sort_by_key(|s| match s.source {
        SchemeSource::Default => 0,
        _ if s.dark == dark_slot => 1,
        _ => 2,
    });
    rows
}

/// Rebuild the picker list and the preview card for the slot being edited.
fn refresh_picker(window: &AppWindow) {
    let dark_slot = window.get_term_scheme_slot_dark();
    let query = window.get_term_scheme_query();
    let list: Vec<TermSchemeItem> = rows(dark_slot, query.as_str()).iter().map(item).collect();
    window.set_term_schemes(ModelRc::new(VecModel::from(list)));

    let current = if dark_slot {
        window.get_term_scheme_dark_id()
    } else {
        window.get_term_scheme_light_id()
    };
    let all = scheme::all(dark_slot);
    if let Some(s) = all
        .iter()
        .find(|s| s.id == current.as_str())
        .or(all.first())
    {
        window.set_term_scheme_preview(item(s));
    }
    // Names of both chosen schemes, for the dark / light mode chips (a
    // scheme's name is the same whichever slot lists it).
    let name = |id: SharedString| -> SharedString {
        all.iter()
            .find(|s| s.id == id.as_str())
            .map(|s| item(s).name)
            .unwrap_or_default()
    };
    window.set_term_scheme_dark_name(name(window.get_term_scheme_dark_id()));
    window.set_term_scheme_light_name(name(window.get_term_scheme_light_id()));
}

/// Push the saved pair into rendering and this window's Theme. Open terminals
/// pick it up on their next re-render (callers re-render when it changed).
pub(super) fn apply_terminal_schemes(window: &AppWindow, store: &ConfigStore) {
    let dark_id = current_id(store, true);
    let light_id = current_id(store, false);
    let (dark, dark_theme) = resolve(true, &dark_id);
    let (light, light_theme) = resolve(false, &light_id);
    scheme::set_active(dark, light);
    window.set_term_colors_dark(dark_theme);
    window.set_term_colors_light(light_theme);
    window.set_term_scheme_dark_id(dark_id.into());
    window.set_term_scheme_light_id(light_id.into());
    refresh_picker(window);
}

/// Save, apply, re-render every terminal in this window and tell the others.
fn commit(
    window: &AppWindow,
    store: &Rc<RefCell<ConfigStore>>,
    bufs: &TermBuffers,
    registry: &Rc<WindowRegistry<slint::Weak<AppWindow>>>,
) {
    let _ = store.borrow().save();
    apply_terminal_schemes(window, &store.borrow());
    apply_dark_mode(window, bufs, window.get_dark_mode());
    registry.broadcast_config_changed();
}

/// Wire the scheme picker callbacks.
pub(super) fn wire(ctx: &WinCtx) {
    let WinCtx {
        store,
        registry,
        bufs,
        window,
        ..
    } = ctx;
    // Open the picker on the mode the app is in.
    window.set_term_scheme_slot_dark(window.get_dark_mode());
    refresh_picker(window);

    {
        let weak = window.as_weak();
        let (store, bufs, registry) = (store.clone(), bufs.clone(), registry.clone());
        window.on_set_term_scheme(move |dark_slot, id: SharedString| {
            let Some(w) = weak.upgrade() else { return };
            let id = if id.as_str() == scheme::DEFAULT_ID {
                ""
            } else {
                id.as_str()
            };
            store.borrow_mut().set_terminal_scheme(dark_slot, id);
            w.set_term_scheme_status("".into());
            commit(&w, &store, &bufs, &registry);
        });
    }
    {
        let weak = window.as_weak();
        window.on_term_scheme_filter(move |dark_slot, query: SharedString| {
            let Some(w) = weak.upgrade() else { return };
            w.set_term_scheme_slot_dark(dark_slot);
            w.set_term_scheme_query(query);
            refresh_picker(&w);
        });
    }
    {
        let weak = window.as_weak();
        let (store, bufs, registry) = (store.clone(), bufs.clone(), registry.clone());
        window.on_import_term_scheme(move |dark_slot| {
            let Some(path) = DialogOwner::of_weak(&weak)
                .file()
                .add_filter("iTerm2 color scheme", &["itermcolors"])
                .pick_file()
            else {
                return;
            };
            let Some(w) = weak.upgrade() else { return };
            let en = crate::i18n::is_en();
            match scheme::import(&path) {
                Ok(id) => {
                    store.borrow_mut().set_terminal_scheme(dark_slot, id);
                    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned());
                    let name = name.unwrap_or_default();
                    w.set_term_scheme_status(
                        if en {
                            format!("Imported {name}")
                        } else {
                            format!("已导入 {name}")
                        }
                        .into(),
                    );
                    commit(&w, &store, &bufs, &registry);
                }
                Err(e) => {
                    w.set_term_scheme_status(
                        if en {
                            format!("Could not import: {e}")
                        } else {
                            format!("导入失败：{e}")
                        }
                        .into(),
                    );
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let (store, bufs, registry) = (store.clone(), bufs.clone(), registry.clone());
        window.on_remove_term_scheme(move |id: SharedString| {
            let Some(w) = weak.upgrade() else { return };
            if !scheme::remove(id.as_str()) {
                return;
            }
            {
                let mut s = store.borrow_mut();
                for dark in [true, false] {
                    if s.terminal_scheme(dark) == id.as_str() {
                        s.set_terminal_scheme(dark, "");
                    }
                }
            }
            w.set_term_scheme_status("".into());
            commit(&w, &store, &bufs, &registry);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_put_default_first_then_the_slots_own_mode() {
        for dark_slot in [true, false] {
            let r = rows(dark_slot, "");
            assert_eq!(r[0].source, SchemeSource::Default);
            let first_other = r
                .iter()
                .position(|s| s.source != SchemeSource::Default && s.dark != dark_slot);
            let last_same = r
                .iter()
                .rposition(|s| s.source != SchemeSource::Default && s.dark == dark_slot);
            assert!(last_same.unwrap() < first_other.unwrap());
        }
    }

    /// Renders Settings › Terminal colours under a few schemes into
    /// `target/term-scheme-*.png` for a visual check:
    /// `cargo test --bin meatshell -- --ignored term_scheme_render`.
    #[test]
    #[ignore]
    fn term_scheme_render() {
        use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
        use slint::{ComponentHandle, Rgb8Pixel};

        struct Backend(std::rc::Rc<MinimalSoftwareWindow>);
        impl slint::platform::Platform for Backend {
            fn create_window_adapter(
                &self,
            ) -> Result<std::rc::Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError>
            {
                Ok(self.0.clone())
            }
        }

        std::thread::spawn(|| {
            let (w, h) = (1280u32, 860u32);
            let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
            slint::platform::set_platform(Box::new(Backend(window.clone()))).unwrap();
            let ui = AppWindow::new().unwrap();
            ui.set_lang_en(false);
            ui.set_interface_start_page("term-colors".into());
            ui.show().unwrap();
            window.set_size(slint::PhysicalSize::new(w, h));
            ui.set_interface_open(true);
            let shots: [(&str, bool, &str); 4] = [
                ("dracula", true, ""),
                ("catppuccin-mocha", true, "cat"),
                ("catppuccin-latte", false, ""),
                ("default", false, ""),
            ];
            for (id, dark, query) in shots {
                ui.set_dark_mode(dark);
                ui.set_term_scheme_slot_dark(dark);
                ui.set_term_scheme_query(query.into());
                if dark {
                    ui.set_term_scheme_dark_id(id.into());
                } else {
                    ui.set_term_scheme_light_id(id.into());
                }
                refresh_picker(&ui);
                for _ in 0..8 {
                    std::thread::sleep(std::time::Duration::from_millis(60));
                    slint::platform::update_timers_and_animations();
                }
                let mut pixels = vec![Rgb8Pixel::default(); (w * h) as usize];
                window.request_redraw();
                window.draw_if_needed(|renderer| {
                    renderer.render(&mut pixels, w as usize);
                });
                let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
                let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join(format!("target/term-scheme-{id}.png"));
                image::save_buffer(path, &bytes, w, h, image::ColorType::Rgb8).unwrap();
            }
        })
        .join()
        .unwrap();
    }

    /// A real pointer click on a scheme row must reach `set-term-scheme`.
    #[test]
    #[ignore]
    fn term_scheme_row_click_reaches_rust() {
        use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
        use slint::platform::{PointerEventButton, WindowEvent};
        use slint::{ComponentHandle, LogicalPosition};

        struct Backend(std::rc::Rc<MinimalSoftwareWindow>);
        impl slint::platform::Platform for Backend {
            fn create_window_adapter(
                &self,
            ) -> Result<std::rc::Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError>
            {
                Ok(self.0.clone())
            }
        }

        let got = std::thread::spawn(|| {
            let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
            slint::platform::set_platform(Box::new(Backend(window.clone()))).unwrap();
            let ui = AppWindow::new().unwrap();
            ui.set_interface_start_page("term-colors".into());
            ui.show().unwrap();
            window.set_size(slint::PhysicalSize::new(1280, 860));
            ui.set_interface_open(true);
            ui.set_term_scheme_slot_dark(true);
            refresh_picker(&ui);
            let got = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
            let g = got.clone();
            ui.on_set_term_scheme(move |dark, id| g.borrow_mut().push(format!("{dark}:{id}")));
            for _ in 0..8 {
                std::thread::sleep(std::time::Duration::from_millis(60));
                slint::platform::update_timers_and_animations();
            }
            let mut px = vec![slint::Rgb8Pixel::default(); 1280 * 860];
            window.request_redraw();
            window.draw_if_needed(|r| {
                r.render(&mut px, 1280);
            });
            // The Dracula row (second row) in the 1280x860 render.
            let position = LogicalPosition::new(700.0, 620.0);
            let w = ui.window();
            w.dispatch_event(WindowEvent::PointerMoved { position });
            w.dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
            for _ in 0..4 {
                std::thread::sleep(std::time::Duration::from_millis(60));
                slint::platform::update_timers_and_animations();
            }
            w.dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
            for _ in 0..4 {
                std::thread::sleep(std::time::Duration::from_millis(60));
                slint::platform::update_timers_and_animations();
            }
            let v = got.borrow().clone();
            v
        })
        .join()
        .unwrap();
        assert_eq!(got, vec!["true:dracula".to_string()]);
    }

    #[test]
    fn search_matches_names_case_insensitively() {
        let r = rows(true, "  CATPPUCCIN ");
        assert_eq!(r.len(), 4);
        assert!(r.iter().all(|s| s.name.starts_with("Catppuccin")));
        assert_eq!(rows(true, "默认")[0].source, SchemeSource::Default);
        assert!(rows(true, "zzz-no-such").is_empty());
    }
}
