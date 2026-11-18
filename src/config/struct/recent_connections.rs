//! Recently opened connections, shown on the welcome page and in the empty
//! main area of the welcome-sidebar layout.
//!
//! Machine-local like `Session::last_used`: the list lives in the config file
//! only, never in exports or WebDAV sync. Entries refer to sessions by id, so
//! renaming a session keeps its place and a deleted session simply stops
//! showing (see `app::recents`).

use serde::{Deserialize, Serialize};

/// How many connections are remembered.
pub const RECENT_CONNECTIONS_CAP: usize = 20;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecentConnection {
    /// Saved session id, or a generated `system:` / `agent:` id.
    pub id: String,
    /// When the connection was opened (Unix seconds).
    pub at: i64,
}

/// Move `id` to the front with time `at`, keeping one entry per session and
/// at most [`RECENT_CONNECTIONS_CAP`] entries.
pub fn record_recent_connection(list: &mut Vec<RecentConnection>, id: &str, at: i64) {
    let id = id.trim();
    if id.is_empty() {
        return;
    }
    list.retain(|entry| entry.id != id);
    list.insert(
        0,
        RecentConnection {
            id: id.to_string(),
            at,
        },
    );
    list.truncate(RECENT_CONNECTIONS_CAP);
}

/// The list as a hand-edited or older config may carry it, cleaned up: no
/// empty ids, one entry per id (the first, i.e. most recent, wins), capped.
pub fn normalized_recent_connections(list: &[RecentConnection]) -> Vec<RecentConnection> {
    let mut seen = std::collections::HashSet::new();
    list.iter()
        .filter(|entry| !entry.id.trim().is_empty() && seen.insert(entry.id.clone()))
        .take(RECENT_CONNECTIONS_CAP)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[RecentConnection]) -> Vec<&str> {
        list.iter().map(|entry| entry.id.as_str()).collect()
    }

    #[test]
    fn reconnecting_moves_the_session_to_the_front_once() {
        let mut list = Vec::new();
        record_recent_connection(&mut list, "a", 1);
        record_recent_connection(&mut list, "b", 2);
        record_recent_connection(&mut list, "a", 3);
        assert_eq!(ids(&list), ["a", "b"]);
        assert_eq!(list[0].at, 3);
    }

    #[test]
    fn the_list_is_capped_and_ignores_empty_ids() {
        let mut list = Vec::new();
        record_recent_connection(&mut list, "  ", 1);
        assert!(list.is_empty());
        for i in 0..(RECENT_CONNECTIONS_CAP + 5) {
            record_recent_connection(&mut list, &format!("s{i}"), i as i64);
        }
        assert_eq!(list.len(), RECENT_CONNECTIONS_CAP);
        assert_eq!(list[0].id, format!("s{}", RECENT_CONNECTIONS_CAP + 4));
    }

    #[test]
    fn hand_edited_lists_are_cleaned_up() {
        let raw = vec![
            RecentConnection { id: "a".into(), at: 5 },
            RecentConnection { id: "".into(), at: 4 },
            RecentConnection { id: "a".into(), at: 3 },
            RecentConnection { id: "b".into(), at: 2 },
        ];
        let clean = normalized_recent_connections(&raw);
        assert_eq!(ids(&clean), ["a", "b"]);
        assert_eq!(clean[0].at, 5);
    }
}
