//! Local (split view) bookmarks and recently opened folders (#532 follow-up).
//!
//! Unlike the remote places (#500), a local folder means the same thing in every
//! session: these are stored once for the whole machine.

use serde::{Deserialize, Serialize};

/// How many recently opened local folders are remembered.
pub const LOCAL_RECENT_CAP: usize = 15;
/// Upper bound on local bookmarks; the oldest is dropped beyond it.
pub const LOCAL_BOOKMARK_CAP: usize = 50;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalPlaces {
    /// In the order they were added.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bookmarks: Vec<String>,
    /// Most recent first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent: Vec<String>,
}

/// Canonical form for comparison and storage: trimmed, absolute, no trailing
/// separator (except a drive root). `None` for anything that is not a folder
/// path this pane could navigate to.
pub fn normalize_local_path(path: &str) -> Option<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() || !std::path::Path::new(trimmed).is_absolute() {
        return None;
    }
    let mut s = trimmed.trim_end_matches(['\\', '/']).to_string();
    // `C:` (a drive-relative path) is not a folder; `C:\` is.
    if s.len() == 2 && s.as_bytes().get(1) == Some(&b':') {
        s.push(std::path::MAIN_SEPARATOR);
    }
    if s.is_empty() {
        return None;
    }
    Some(s)
}

/// Comparison key: Windows folders are the same regardless of case, so the
/// lists are de-duplicated the way the OS would.
fn place_key(path: &str) -> String {
    if cfg!(windows) {
        path.to_lowercase()
    } else {
        path.to_string()
    }
}

fn dedup(mut paths: Vec<String>, cap: usize) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    paths.retain(|path| seen.insert(place_key(path)));
    paths.truncate(cap);
    paths
}

impl LocalPlaces {
    pub fn is_empty(&self) -> bool {
        self.bookmarks.is_empty() && self.recent.is_empty()
    }

    /// Normalized, de-duplicated lists with the caps applied (a hand-edited or
    /// older config can carry anything).
    pub fn normalized(&self) -> Self {
        let clean = |paths: &[String]| -> Vec<String> {
            paths
                .iter()
                .filter_map(|path| normalize_local_path(path))
                .collect()
        };
        Self {
            bookmarks: dedup(clean(&self.bookmarks), LOCAL_BOOKMARK_CAP),
            recent: dedup(clean(&self.recent), LOCAL_RECENT_CAP),
        }
    }

    /// Move `path` to the front of the recent list. Returns whether anything
    /// changed, so re-listing the folder already on top is a no-op.
    pub fn record_visit(&mut self, path: &str) -> bool {
        let Some(path) = normalize_local_path(path) else {
            return false;
        };
        let key = place_key(&path);
        if self
            .recent
            .first()
            .is_some_and(|first| place_key(first) == key)
        {
            return false;
        }
        self.recent.retain(|entry| place_key(entry) != key);
        self.recent.insert(0, path);
        self.recent.truncate(LOCAL_RECENT_CAP);
        true
    }

    pub fn is_bookmarked(&self, path: &str) -> bool {
        let Some(path) = normalize_local_path(path) else {
            return false;
        };
        let key = place_key(&path);
        self.bookmarks
            .iter()
            .any(|entry| place_key(entry) == key)
    }

    /// Add or remove `path`; returns whether it is bookmarked afterwards.
    pub fn toggle_bookmark(&mut self, path: &str) -> bool {
        if self.is_bookmarked(path) {
            self.remove_bookmark(path);
            false
        } else {
            let Some(path) = normalize_local_path(path) else {
                return false;
            };
            self.bookmarks.insert(0, path);
            self.bookmarks.truncate(LOCAL_BOOKMARK_CAP);
            true
        }
    }

    pub fn remove_bookmark(&mut self, path: &str) {
        let Some(path) = normalize_local_path(path) else {
            return;
        };
        let key = place_key(&path);
        self.bookmarks.retain(|entry| place_key(entry) != key);
    }

    pub fn clear_recent(&mut self) {
        self.recent.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn windows_path() -> String {
        if cfg!(windows) {
            r"C:\Users\me\work".to_string()
        } else {
            "/home/me/work".to_string()
        }
    }

    #[test]
    fn only_absolute_folders_are_kept() {
        assert_eq!(normalize_local_path(""), None);
        assert_eq!(normalize_local_path("   "), None);
        assert_eq!(normalize_local_path("relative/dir"), None);
        assert!(normalize_local_path(&windows_path()).is_some());
    }

    #[test]
    fn trailing_separators_are_dropped_but_the_root_stays() {
        assert_eq!(
            normalize_local_path(&format!("{}//", windows_path())),
            Some(windows_path())
        );
        #[cfg(windows)]
        {
            assert_eq!(normalize_local_path(r"C:\"), Some(r"C:\".to_string()));
            assert_eq!(normalize_local_path("C:"), None);
            assert_eq!(normalize_local_path(r"D:\\"), Some(r"D:\".to_string()));
        }
    }

    #[test]
    fn record_visit_moves_to_the_front_and_caps_the_list() {
        let mut places = LocalPlaces::default();
        assert!(places.record_visit(&windows_path()));
        // Re-listing the same folder (or the same folder with another
        // separator) changes nothing.
        assert!(!places.record_visit(&windows_path()));
        assert!(!places.record_visit(&format!("{}/", windows_path())));
        assert_eq!(places.recent.len(), 1);

        for index in 0..(LOCAL_RECENT_CAP + 5) {
            places.record_visit(&format!("{}/sub{index}", windows_path()));
        }
        assert_eq!(places.recent.len(), LOCAL_RECENT_CAP);
        assert!(places.recent[0].ends_with("sub19"), "{:?}", places.recent);
    }

    #[test]
    fn bookmarks_toggle_remove_and_dedup() {
        let mut places = LocalPlaces::default();
        assert!(!places.is_bookmarked(&windows_path()));
        assert!(places.toggle_bookmark(&windows_path()));
        assert!(places.is_bookmarked(&windows_path()));
        // Same folder, different separator/case on Windows → still one entry.
        assert!(!places.toggle_bookmark(&format!("{}/", windows_path())));
        assert!(!places.is_bookmarked(&windows_path()));

        places.toggle_bookmark(&windows_path());
        places.remove_bookmark(&windows_path());
        assert!(places.bookmarks.is_empty());
    }

    #[test]
    fn normalized_lists_clean_hand_edited_configs() {
        let raw = LocalPlaces {
            bookmarks: vec![
                windows_path(),
                format!("{}/", windows_path()),
                "relative".into(),
            ],
            recent: vec![windows_path(), windows_path()],
        };
        let clean = raw.normalized();
        assert_eq!(clean.bookmarks.len(), 1);
        assert_eq!(clean.recent.len(), 1);
        assert!(!clean.is_empty());

        let empty = LocalPlaces::default();
        assert!(empty.is_empty());
        let mut cleared = cleaned_up();
        cleared.clear_recent();
        assert!(cleared.recent.is_empty());
    }

    fn cleaned_up() -> LocalPlaces {
        LocalPlaces {
            recent: vec![windows_path()],
            ..Default::default()
        }
    }
}
