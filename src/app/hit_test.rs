//! Geometry and hit-testing of the main window's terminal and SFTP areas.

use crate::terminal::ScreenDims as _;
use super::*;

/// Width of a left/right-docked SFTP panel as terminal_view.slint lays it out:
/// never narrower than 300px, whatever width was saved (#505).
fn side_sftp_width(saved: f32) -> f32 {
    saved.max(300.0)
}

/// The active terminal tab's current SFTP directory ("" if unknown).
pub(super) fn active_sftp_path(win: &AppWindow, tab_id: &str) -> String {
    let model = win.get_terminals();
    if let Some(m) = model.as_any().downcast_ref::<VecModel<TerminalState>>() {
        for i in 0..m.row_count() {
            if let Some(row) = m.row_data(i) {
                if row.id.as_str() == tab_id {
                    return row.sftp_path.to_string();
                }
            }
        }
    }
    String::new()
}

// The raw macOS wheel fallback runs before the usual Slint hit testing. Keep
// modal-state routing explicit so it cannot target a terminal behind a dialog.
pub(super) fn macos_terminal_wheel_can_target_terminal(modal_open: bool) -> bool {
    !modal_open
}

/// Which tab's terminal output the pointer is over, as Slint's own hit
/// testing reports it (#540).
///
/// `terminal_wheel_hit` rebuilds the terminal's rectangle by mirroring the
/// window layout; any mismatch let the macOS fallback claim wheel events over
/// the neighbouring SFTP file list, so its file-name column would not scroll.
/// The fallback now claims a wheel only while this says the active tab's
/// output is under the pointer; the mirrored geometry merely maps the pointer
/// to a terminal cell.
#[derive(Default)]
pub(super) struct HoveredTerminal(RefCell<Option<String>>);

impl HoveredTerminal {
    /// Record a hover change. Enter/leave between two tabs can arrive in either
    /// order, so a leave only clears the tab that left.
    pub(super) fn set(&self, tab_id: &str, hovered: bool) {
        let mut current = self.0.borrow_mut();
        if hovered {
            *current = Some(tab_id.to_string());
        } else if current.as_deref() == Some(tab_id) {
            *current = None;
        }
    }

    /// Whether the pointer is over `active_tab`'s terminal output.
    pub(super) fn is(&self, active_tab: &str) -> bool {
        self.0.borrow().as_deref() == Some(active_tab)
    }
}

#[cfg(test)]
mod hovered_terminal_tests {
    use super::HoveredTerminal;

    #[test]
    fn only_the_hovered_active_terminal_may_claim_the_wheel() {
        let hovered = HoveredTerminal::default();
        assert!(!hovered.is("t1"), "nothing hovered yet: never claim");
        hovered.set("t1", true);
        assert!(hovered.is("t1"));
        assert!(!hovered.is("t2"), "another pane's tab is not the active one");
        // Pointer moved onto the SFTP file list next to the terminal.
        hovered.set("t1", false);
        assert!(!hovered.is("t1"));
    }

    #[test]
    fn enter_before_leave_between_panes_keeps_the_new_tab() {
        let hovered = HoveredTerminal::default();
        hovered.set("t1", true);
        hovered.set("t2", true); // entered t2 before t1's leave arrived
        hovered.set("t1", false);
        assert!(hovered.is("t2"));
    }
}

pub(super) fn terminal_wheel_hit(
    win: &AppWindow,
    bufs: &TermBuffers,
    x: f32,
    y: f32,
) -> Option<TerminalWheelHit> {
    let (active, term, term_state) = active_terminal_panel_rects(win)?;
    let mut term_x = term.x;
    let mut term_y = term.y;
    let mut term_w = term.w;
    let mut term_h = term.h;

    // Zen mode / terminal fullscreen remove the 24px status strip; the
    // command bar stays on screen.
    if !panels_hidden(win) {
        term_y += 24.0;
        term_h = (term_h - 24.0).max(0.0);
    }

    let sftp_dock = win.get_sftp_dock().to_string();
    let sftp_take = if term_state.sftp_collapsed {
        36.0
    } else if sftp_dock == "left" || sftp_dock == "right" {
        side_sftp_width(term_state.sftp_panel_width) + 4.0
    } else {
        term_state.sftp_panel_height + 4.0
    };
    shrink_edge(
        &mut term_x,
        &mut term_y,
        &mut term_w,
        &mut term_h,
        &sftp_dock,
        sftp_take,
    );

    // Leave the command bar to TextInput/history handling; wheel fallback is
    // for terminal output only. The bar is present in every mode unless the
    // toolbar toggle hid it.
    if !win.get_cmd_bar_hidden() {
        term_h = (term_h - 34.0).max(0.0);
    }
    if !contains_logical(
        LogicalRect {
            x: term_x,
            y: term_y,
            w: term_w,
            h: term_h,
        },
        x,
        y,
    ) {
        return None;
    }

    let h = term_buf(bufs, &active)?;
    let guard = h.lock().ok()?;
    let screen = guard.parser.screen();
    let (rows, cols) = screen.dims();
    let cell_w = (term_w / cols.max(1) as f32).max(1.0);
    let cell_h = (term_h / rows.max(1) as f32).max(1.0);
    Some(TerminalWheelHit {
        tab_id: active,
        is_alt: screen.alternate_screen(),
        col: ((x - term_x) / cell_w).floor() as i32,
        row: ((y - term_y) / cell_h).floor() as i32,
    })
}

pub(super) fn shrink_edge(
    x: &mut f32,
    y: &mut f32,
    w: &mut f32,
    h: &mut f32,
    dock: &str,
    amount: f32,
) {
    let amount = amount.max(0.0);
    match dock {
        "left" => {
            *x += amount;
            *w = (*w - amount).max(0.0);
        }
        "right" => *w = (*w - amount).max(0.0),
        "top" => {
            *y += amount;
            *h = (*h - amount).max(0.0);
        }
        "bottom" => *h = (*h - amount).max(0.0),
        _ => {}
    }
}

pub(super) fn contains_logical(rect: LogicalRect, x: f32, y: f32) -> bool {
    x >= rect.x && x <= rect.x + rect.w && y >= rect.y && y <= rect.y + rect.h
}

/// Zen mode and terminal fullscreen (#495) both hide every docked panel and
/// the terminal status strip — mirrors `hide-panels` in app.slint.
pub(super) fn panels_hidden(win: &AppWindow) -> bool {
    win.get_zen_mode() || win.get_term_fullscreen()
}

pub(super) fn app_content_area(win: &AppWindow) -> LogicalRect {
    let size = win.window().size();
    let scale = win.window().scale_factor().max(0.01) as f32;
    let mut area = LogicalRect {
        x: 0.0,
        // Terminal fullscreen drops the title bar / macOS inset (#495).
        y: if win.get_term_fullscreen() {
            0.0
        } else if win.get_custom_titlebar() {
            38.0
        } else if win.get_is_mac() {
            28.0
        } else {
            0.0
        },
        w: size.width as f32 / scale,
        h: 0.0,
    };
    area.h = size.height as f32 / scale - area.y;

    if panels_hidden(win) {
        return area;
    }

    if win.get_welcome_as_sidebar() {
        let dock = win.get_welcome_sidebar_dock().to_string();
        let sidebar_strip_outside = !win.get_welcome_collapsed()
            && win.get_sidebar_collapsed()
            && win.get_sidebar_dock().as_str() == dock.as_str();
        let welcome_taken = (if win.get_welcome_collapsed() {
            36.0
        } else {
            win.get_welcome_sidebar_width()
        }) + if sidebar_strip_outside { 36.0 } else { 0.0 };
        shrink_edge(
            &mut area.x,
            &mut area.y,
            &mut area.w,
            &mut area.h,
            &dock,
            welcome_taken,
        );
    }

    let side_dock = win.get_sidebar_dock().to_string();
    let side_take = if win.get_sidebar_collapsed() {
        36.0
    } else if side_dock == "left" || side_dock == "right" {
        win.get_sidebar_width() + 4.0
    } else {
        win.get_sidebar_height() + 4.0
    };
    shrink_edge(
        &mut area.x,
        &mut area.y,
        &mut area.w,
        &mut area.h,
        &side_dock,
        side_take,
    );
    if win.get_quick_panel_open() {
        let quick_dock = win.get_quick_panel_dock().to_string();
        let quick_merged = win.get_quick_panel_collapsed()
            && ((win.get_welcome_as_sidebar()
                && win.get_welcome_collapsed()
                && win.get_welcome_sidebar_dock().as_str() == quick_dock.as_str())
                || (win.get_sidebar_collapsed() && side_dock.as_str() == quick_dock.as_str()));
        if quick_merged {
            return area;
        }
        let quick_take = if win.get_quick_panel_collapsed() {
            36.0
        } else if quick_dock == "left" || quick_dock == "right" {
            win.get_quick_panel_width() + 4.0
        } else {
            win.get_quick_panel_height() + 4.0
        };
        shrink_edge(
            &mut area.x,
            &mut area.y,
            &mut area.w,
            &mut area.h,
            &quick_dock,
            quick_take,
        );
    }
    area
}

pub(super) fn active_terminal_panel_rects(
    win: &AppWindow,
) -> Option<(String, LogicalRect, TerminalState)> {
    let active = win.get_active_tab_id().to_string();
    if active.is_empty() || active == "welcome" {
        return None;
    }

    let area = app_content_area(win);
    let panes = win.get_panes();
    let pane = (0..panes.row_count())
        .filter_map(|i| panes.row_data(i))
        .find(|p| p.active_id.as_str() == active.as_str())?;

    let terms = win.get_terminals();
    let term_state = (0..terms.row_count())
        .filter_map(|i| terms.row_data(i))
        .find(|t| t.id.as_str() == active.as_str())?;

    // Terminal fullscreen hides the 36px tab strip (#495).
    let top = if win.get_term_fullscreen() { 4.0 } else { 40.0 };
    Some((
        active,
        LogicalRect {
            x: area.x + pane.x,
            y: area.y + pane.y + top,
            w: pane.w,
            h: (pane.h - top).max(0.0),
        },
        term_state,
    ))
}

/// The split view's local-pane rectangle for the active terminal tab, or `None`
/// when the panel is hidden, collapsed, or the tab is not in the split view.
///
/// Mirrors the layout in `ui/terminal_view.slint` (24px status strip, docked
/// panel at the chosen edge) and `ui/sftp_panel.slint` (toolbar + content +
/// 18px status bar; the local half is `local-w` wide). Used to route OS file
/// drops: dropping on the local half copies files into that local folder,
/// dropping anywhere else uploads them to the remote directory.
pub(super) fn active_sftp_local_pane_rect(win: &AppWindow) -> Option<(String, LogicalRect)> {
    if panels_hidden(win) {
        return None;
    }
    let (active, term, state) = active_terminal_panel_rects(win)?;
    if state.sftp_collapsed || !state.sftp_dual {
        return None;
    }

    // TerminalView starts with a 24px connection-status strip; the SFTP panel
    // is docked inside the region below it.
    let region = LogicalRect {
        x: term.x,
        y: term.y + 24.0,
        w: term.w,
        h: (term.h - 24.0).max(0.0),
    };
    let dock = win.get_sftp_dock().to_string();
    let vertical = dock == "left" || dock == "right";
    let panel_w = if vertical {
        side_sftp_width(state.sftp_panel_width)
    } else {
        region.w
    };
    let panel_h = if vertical {
        region.h
    } else {
        state.sftp_panel_height
    };
    let panel = LogicalRect {
        x: if dock == "right" {
            region.x + (region.w - panel_w).max(0.0)
        } else {
            region.x
        },
        y: if dock == "bottom" {
            region.y + (region.h - panel_h).max(0.0)
        } else {
            region.y
        },
        w: panel_w,
        h: panel_h,
    };

    // SftpPanel: toolbar (62px in the two-row compact layout), content, 18px
    // status bar. The split view hides the tree, so the compact threshold is
    // the bare 560px.
    let toolbar = if panel.w < 560.0 { 62.0 } else { 34.0 };
    let content_h = (panel.h - toolbar - 18.0).max(0.0);
    // Mirrors `local-w` in ui/sftp_panel.slint: the dragged width, else the
    // remembered share, else an even split.
    let ratio = win.get_sftp_local_split();
    let wanted = if state.sftp_local_pane_width > 0.0 {
        state.sftp_local_pane_width
    } else if ratio > 0.0 {
        panel.w * ratio
    } else {
        panel.w / 2.0
    };
    let local_w = wanted.clamp(160.0, (panel.w - 200.0).max(160.0));

    Some((
        active,
        LogicalRect {
            x: panel.x,
            y: panel.y + toolbar,
            w: local_w,
            h: content_h,
        },
    ))
}

/// `(tab id, local folder)` when `(x, y)` is over the split view's local pane.
pub(super) fn sftp_local_pane_hit(win: &AppWindow, x: f32, y: f32) -> Option<(String, String)> {
    let (tab, rect) = active_sftp_local_pane_rect(win)?;
    if !contains_logical(rect, x, y) {
        return None;
    }
    let dir = current_path(win, &tab);
    (!dir.is_empty()).then_some((tab, dir))
}
