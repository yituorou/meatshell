//! Recent connections: the floating "Recent" card in the empty main area of
//! the welcome-sidebar layout, and the shelf of tiles on the full welcome page.
//!
//! The history itself is `ConfigStore::recent_connections` (session ids with a
//! time). This module turns it into display rows, skipping sessions that no
//! longer exist, and keeps the relative times ("5 min ago") fresh: the UI asks
//! for a refresh once a minute while a recents view is on screen.

use std::cell::RefCell;
use std::rc::Rc;

use chrono::{DateTime, Datelike, Local, TimeZone};
use slint::{ComponentHandle as _, Model as _, ModelRc, VecModel};

use crate::config::{ConfigStore, RecentConnection, Session, SessionKind};
use crate::i18n::{is_en, t};
use crate::ui::{AppWindow, RecentSession};

/// How many recent connections the card and the shelf show.
const SHOWN: usize = 6;

/// Rebuild the window's recents model from the store.
pub(super) fn publish(window: &AppWindow, store: &ConfigStore) {
    let rows = rows(store, Local::now());
    window.set_recent_sessions(ModelRc::new(VecModel::from(rows)));
}

/// Remember that `id` was opened, save, and show it at the top right away.
pub(super) fn record(window: &AppWindow, store: &RefCell<ConfigStore>, id: &str) {
    {
        let mut s = store.borrow_mut();
        s.record_connection(id);
        if let Err(error) = s.save() {
            tracing::warn!("failed to save recent connections: {error:#}");
        }
    }
    publish(window, &store.borrow());
}

pub(super) fn wire(window: &AppWindow, store: Rc<RefCell<ConfigStore>>) {
    {
        let weak = window.as_weak();
        let store = store.clone();
        window.on_remove_recent(move |id| {
            let Some(w) = weak.upgrade() else { return };
            let mut s = store.borrow_mut();
            s.remove_recent_connection(id.as_str());
            if let Err(error) = s.save() {
                tracing::warn!("failed to save recent connections: {error:#}");
            }
            publish(&w, &s);
        });
    }
    {
        let weak = window.as_weak();
        let store = store.clone();
        window.on_clear_recents(move || {
            let Some(w) = weak.upgrade() else { return };
            let mut s = store.borrow_mut();
            s.clear_recent_connections();
            if let Err(error) = s.save() {
                tracing::warn!("failed to save recent connections: {error:#}");
            }
            publish(&w, &s);
        });
    }
    {
        let weak = window.as_weak();
        window.on_refresh_recents(move || {
            if let Some(w) = weak.upgrade() {
                refresh(&w, &store.borrow());
            }
        });
    }
}

/// Minute tick: update the relative times in place. A connection opened in
/// another window lands at the front of the list, so a different first entry
/// gets a full rebuild instead.
fn refresh(window: &AppWindow, store: &ConfigStore) {
    let model = window.get_recent_sessions();
    let entries = store.recent_connections();
    let now = Local::now();
    let front_shown = model.row_data(0).map(|row| row.id.to_string());
    if front_shown.as_deref() != entries.first().map(|entry| entry.id.as_str()) {
        publish(window, store);
        return;
    }
    for i in 0..model.row_count() {
        let Some(mut row) = model.row_data(i) else { continue };
        if let Some(entry) = entries.iter().find(|entry| entry.id == row.id.as_str()) {
            let when = when_label(at_local(entry.at), now, is_en());
            if row.when.as_str() != when {
                row.when = when.into();
                model.set_row_data(i, row);
            }
        }
    }
}

fn rows(store: &ConfigStore, now: DateTime<Local>) -> Vec<RecentSession> {
    let entries = store.recent_connections();
    // Generated shells and agents are only looked up when one is in the list:
    // finding them scans the disk.
    let generated: Vec<Session> = if entries.iter().any(|entry| is_generated(&entry.id)) {
        super::session_models::quick_connect_builtin_sessions(store)
    } else {
        Vec::new()
    };
    entries
        .iter()
        .filter_map(|entry| {
            let session = if is_generated(&entry.id) {
                generated.iter().find(|s| s.id == entry.id)
            } else {
                store.get(&entry.id)
            }?;
            Some(row(session, entry, now))
        })
        .take(SHOWN)
        .collect()
}

fn is_generated(id: &str) -> bool {
    id.starts_with("system:") || id.starts_with(super::agents::AGENT_ID_PREFIX)
}

fn row(session: &Session, entry: &RecentConnection, now: DateTime<Local>) -> RecentSession {
    RecentSession {
        id: entry.id.clone().into(),
        name: session.name.clone().into(),
        detail: detail(session).into(),
        when: when_label(at_local(entry.at), now, is_en()).into(),
        icon: icon(session, &entry.id).into(),
    }
}

/// Second line under the name: where the connection goes.
fn detail(session: &Session) -> String {
    let at_host = |default_port: u16| {
        let host = if session.port == 0 || session.port == default_port {
            session.host.clone()
        } else {
            format!("{}:{}", session.host, session.port)
        };
        if session.user.is_empty() {
            host
        } else {
            format!("{}@{host}", session.user)
        }
    };
    match session.kind {
        SessionKind::Ssh => at_host(22),
        SessionKind::Rdp => at_host(3389),
        SessionKind::Telnet => format!("telnet {}", at_host(23)),
        SessionKind::Serial => format!("{} · {}", session.serial_port, session.baud_rate),
        // Agents keep the program they run in `note` (see agents.rs).
        SessionKind::Local if !session.note.is_empty() => session.note.clone(),
        SessionKind::Local => t("本地终端", "Local shell").to_string(),
    }
}

/// Material Icons glyph for the row's tile.
fn icon(session: &Session, id: &str) -> &'static str {
    if id.starts_with(super::agents::AGENT_ID_PREFIX) {
        return "\u{F06C}"; // smart_toy
    }
    match session.kind {
        SessionKind::Ssh => "\u{E875}",    // dns
        SessionKind::Local => "\u{EB8E}",  // terminal
        SessionKind::Rdp => "\u{E30C}",    // desktop_windows
        SessionKind::Telnet => "\u{EB2F}", // lan
        SessionKind::Serial => "\u{EFE6}", // cable
    }
}

fn at_local(at: i64) -> DateTime<Local> {
    Local
        .timestamp_opt(at, 0)
        .single()
        .unwrap_or_else(Local::now)
}

/// When a connection was opened, the way Apple's apps say it: "Just now",
/// "12 min ago", a time today, "Yesterday", the weekday within a week, then
/// the date (with the year once it is not this year's).
fn when_label<Tz: TimeZone>(then: DateTime<Tz>, now: DateTime<Tz>, en: bool) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let seconds = (now.clone() - then.clone()).num_seconds().max(0);
    if seconds < 60 {
        return if en { "Just now" } else { "刚刚" }.to_string();
    }
    if seconds < 3600 {
        let minutes = seconds / 60;
        return if en {
            format!("{minutes} min ago")
        } else {
            format!("{minutes} 分钟前")
        };
    }
    let (then_day, today) = (then.date_naive(), now.date_naive());
    if then_day == today {
        return then.format("%H:%M").to_string();
    }
    if today.pred_opt() == Some(then_day) {
        return if en { "Yesterday" } else { "昨天" }.to_string();
    }
    let days = (today - then_day).num_days();
    if (0..7).contains(&days) {
        return weekday(then.weekday(), en).to_string();
    }
    let same_year = then_day.year() == today.year();
    match (en, same_year) {
        (true, true) => then.format("%b %-d").to_string(),
        (true, false) => then.format("%b %-d, %Y").to_string(),
        (false, true) => format!("{}月{}日", then_day.month(), then_day.day()),
        (false, false) => format!("{}年{}月{}日", then_day.year(), then_day.month(), then_day.day()),
    }
}

fn weekday(day: chrono::Weekday, en: bool) -> &'static str {
    use chrono::Weekday::*;
    match (day, en) {
        (Mon, true) => "Monday",
        (Tue, true) => "Tuesday",
        (Wed, true) => "Wednesday",
        (Thu, true) => "Thursday",
        (Fri, true) => "Friday",
        (Sat, true) => "Saturday",
        (Sun, true) => "Sunday",
        (Mon, false) => "星期一",
        (Tue, false) => "星期二",
        (Wed, false) => "星期三",
        (Thu, false) => "星期四",
        (Fri, false) => "星期五",
        (Sat, false) => "星期六",
        (Sun, false) => "星期日",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<FixedOffset> {
        FixedOffset::east_opt(8 * 3600)
            .unwrap()
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .unwrap()
    }

    #[test]
    fn times_read_like_apple_apps() {
        // Wednesday 18 Nov 2026, 22:30.
        let now = at(2026, 11, 18, 22, 30);
        let label = |then, en| when_label(then, now, en);
        assert_eq!(label(at(2026, 11, 18, 22, 30), true), "Just now");
        assert_eq!(label(at(2026, 11, 18, 22, 18), true), "12 min ago");
        assert_eq!(label(at(2026, 11, 18, 22, 18), false), "12 分钟前");
        assert_eq!(label(at(2026, 11, 18, 9, 5), true), "09:05");
        assert_eq!(label(at(2026, 11, 17, 23, 50), true), "Yesterday");
        assert_eq!(label(at(2026, 11, 17, 23, 50), false), "昨天");
        assert_eq!(label(at(2026, 11, 14, 10, 0), true), "Saturday");
        assert_eq!(label(at(2026, 11, 14, 10, 0), false), "星期六");
        assert_eq!(label(at(2026, 11, 3, 10, 0), true), "Nov 3");
        assert_eq!(label(at(2026, 11, 3, 10, 0), false), "11月3日");
        assert_eq!(label(at(2025, 12, 24, 10, 0), true), "Dec 24, 2025");
        assert_eq!(label(at(2025, 12, 24, 10, 0), false), "2025年12月24日");
        // A clock that moved backwards never shows a negative time.
        assert_eq!(label(at(2026, 11, 18, 23, 0), true), "Just now");
    }

    #[test]
    fn details_hide_default_ports() {
        let mut ssh = Session::new_empty();
        ssh.user = "jeff".into();
        ssh.host = "192.168.100.41".into();
        ssh.port = 22;
        assert_eq!(detail(&ssh), "jeff@192.168.100.41");
        ssh.port = 2222;
        assert_eq!(detail(&ssh), "jeff@192.168.100.41:2222");

        let mut rdp = ssh.clone();
        rdp.kind = SessionKind::Rdp;
        rdp.port = 3389;
        assert_eq!(detail(&rdp), "jeff@192.168.100.41");
        assert_eq!(icon(&rdp, "x"), "\u{E30C}");
    }
}
