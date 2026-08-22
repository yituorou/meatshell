//! Session / application configuration.
//!
//! Persists a simple JSON file in the app's data directory. Resolution is
//! **portable-first** (#141): a `config/` folder next to the executable is
//! preferred so the whole app can ride along on a USB stick and never litters
//! the user profile. When the executable lives somewhere read-only (a
//! system-wide install under Program Files / `/usr`), it falls back to the
//! per-user OS config dir (e.g. `%APPDATA%/meatshell`, `~/.config/meatshell`),
//! which is also where every pre-0.4.15 version stored its data — so existing
//! installs keep working untouched. See [`data_dir`].
//!
//! ## Password encryption
//!
//! Passwords are **not** stored in plaintext.  On first launch a random
//! 256-bit key is written to `secret.key` in the same config directory
//! (mode `0600` on Unix).  Every non-empty password is then encrypted with
//! **ChaCha20-Poly1305** (a random 96-bit nonce per value) and stored as
//!
//! ```text
//! enc:v1:<base64url(nonce_12_bytes || ciphertext)>
//! ```
//!
//! Legacy plaintext passwords (from older installs) are left untouched in
//! memory and silently re-encrypted the next time the config is saved.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chacha20poly1305::{
    aead::{Aead, AeadCore, KeyInit},
    ChaCha20Poly1305,
};
use directories::ProjectDirs;
use rand::rngs::OsRng;
use uuid::Uuid;

use super::structs::*;

// ── Data directory resolution (portable-first, #141) ──────────────────────────
//
// All user data — sessions.json, secret.key, known_hosts, error.log — lives in
// ONE directory resolved here, and `errlog` / `known_hosts` route through it too.

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

/// The single directory holding all user data (sessions, encryption key,
/// known_hosts, error.log). Resolved once and cached; any one-time migration
/// from the legacy per-user dir runs exactly once.
///
/// Portable-first: prefers a `config/` folder beside the executable, falling
/// back to the per-user OS config dir when the exe dir is read-only (#141).
pub fn data_dir() -> PathBuf {
    DATA_DIR.get_or_init(resolve_data_dir).clone()
}

/// Directory for diagnostic logs (`error.log`). Kept *separate* from the config
/// dir so logs don't clutter user data: portable-first → a `log/` folder beside
/// the executable (a sibling of `config/`), falling back to a `log/` subdir
/// under the per-user data dir when the exe dir is read-only (Program Files etc.)
/// (#log-dir).
pub fn log_dir() -> PathBuf {
    // Portable: <exe_dir>/log, sibling of the portable config/ folder.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let log = parent.join("log");
            if fs::create_dir_all(&log).is_ok() && dir_is_writable(&log) {
                return log;
            }
        }
    }
    // Read-only exe dir → put logs in their own subdir under the per-user data
    // dir (still not mixed in with sessions.json et al.).
    let dir = data_dir().join("log");
    let _ = fs::create_dir_all(&dir);
    dir
}

/// Pre-0.4.15 location: the per-user OS config dir
/// (`%APPDATA%/meatshell`, `~/.config/meatshell`, …).
fn legacy_data_dir() -> Option<PathBuf> {
    ProjectDirs::from("dev", "meatshell", "meatshell").map(|d| d.config_dir().to_path_buf())
}

/// Portable location: a `config/` folder beside the executable.
fn portable_data_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("config"))
}

/// True only if we can actually create and write a file in `dir` — Program Files
/// and other system locations can reject writes even when the dir appears to
/// exist, so a real write probe is the reliable test.
fn dir_is_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".write_probe_{}", std::process::id()));
    match fs::write(&probe, b"") {
        Ok(()) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn resolve_data_dir() -> PathBuf {
    let legacy = legacy_data_dir();

    if let Some(portable) = portable_data_dir() {
        // Already a portable install → keep using it (nothing to migrate).
        if portable.exists() && dir_is_writable(&portable) {
            return portable;
        }
        // Otherwise try to claim the portable dir. This succeeds only where the
        // exe directory is writable (i.e. not a Program Files / system install),
        // which is exactly when portable mode makes sense.
        if fs::create_dir_all(&portable).is_ok() && dir_is_writable(&portable) {
            if let Some(ref legacy) = legacy {
                migrate_legacy(legacy, &portable);
            }
            return portable;
        }
    }

    // Fall back to the legacy per-user dir (also the pre-0.4.15 location). Last
    // resort: a temp dir, so the app still launches if neither is available.
    let dir = legacy.unwrap_or_else(|| std::env::temp_dir().join("meatshell"));
    let _ = fs::create_dir_all(&dir);
    dir
}

/// On the first launch that lands on the portable dir, copy user data over from
/// the legacy per-user dir so upgrading users keep their saved sessions. The
/// originals are left in place (copy, not move) as a safety net, and existing
/// destination files are never overwritten (#141).
fn migrate_legacy(legacy: &Path, portable: &Path) {
    if legacy == portable {
        return;
    }
    for name in ["sessions.json", "secret.key", "known_hosts"] {
        let src = legacy.join(name);
        let dst = portable.join(name);
        if src.exists() && !dst.exists() {
            match fs::copy(&src, &dst) {
                Ok(_) => {
                    // Keep the key owner-only on Unix (copy preserves bytes, not
                    // necessarily the mode).
                    #[cfg(unix)]
                    if name == "secret.key" {
                        use std::os::unix::fs::PermissionsExt;
                        let _ = fs::set_permissions(&dst, fs::Permissions::from_mode(0o600));
                    }
                    tracing::info!(
                        "migrated {name} to portable config dir {}",
                        portable.display()
                    );
                }
                Err(e) => tracing::warn!(
                    "data migration: failed to copy {} → {}: {e}",
                    src.display(),
                    dst.display()
                ),
            }
        }
    }
}

fn sessions_file_has_connections(path: &Path) -> bool {
    let Ok(raw) = fs::read_to_string(path) else {
        return false;
    };
    serde_json::from_str::<ConfigFile>(&raw)
        .map(|cfg| !cfg.sessions.is_empty())
        .unwrap_or(false)
}

fn restore_user_backup_if_needed(primary_dir: &Path, backup_dir: &Path) {
    if primary_dir == backup_dir {
        return;
    }
    let primary_sessions = primary_dir.join("sessions.json");
    let backup_sessions = backup_dir.join("sessions.json");
    if sessions_file_has_connections(&primary_sessions)
        || !sessions_file_has_connections(&backup_sessions)
    {
        return;
    }
    let _ = fs::create_dir_all(primary_dir);
    for name in ["sessions.json", "secret.key", "known_hosts"] {
        let src = backup_dir.join(name);
        let dst = primary_dir.join(name);
        if src.exists() {
            match fs::copy(&src, &dst) {
                Ok(_) => {
                    #[cfg(unix)]
                    if name == "secret.key" {
                        use std::os::unix::fs::PermissionsExt;
                        let _ = fs::set_permissions(&dst, fs::Permissions::from_mode(0o600));
                    }
                    tracing::info!(
                        "restored {name} from user config backup {}",
                        backup_dir.display()
                    );
                }
                Err(e) => tracing::warn!(
                    "failed to restore {} from {} to {}: {e}",
                    name,
                    src.display(),
                    dst.display()
                ),
            }
        }
    }
}

fn normalize_hex_color(value: &str) -> Option<String> {
    let digits = value.trim().strip_prefix('#').unwrap_or(value.trim());
    if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("#{}", digits.to_ascii_uppercase()))
}

/// A brand-new config (no file yet, or the old one was corrupt). Seeds the
/// new-user default layout (#new-user-defaults): ms wallpaper, welcome page as
/// a left sidebar, resource panel docked right, 15% wallpaper transparency, and
/// marks the migration done so it isn't re-applied.
fn fresh_config() -> ConfigFile {
    ConfigFile {
        wallpaper: "builtin:ms".to_string(),
        welcome_as_sidebar: true,
        sidebar_dock: "right".to_string(),
        wallpaper_overlay: DEFAULT_WALLPAPER_OVERLAY,
        defaults_rev: DEFAULTS_REV,
        ..ConfigFile::default()
    }
}

/// One-time push of the new default layout to *existing* users — but only for
/// each item they're still leaving at the old default, so deliberate choices are
/// never clobbered. Runs once (gated by `defaults_rev`); returns whether anything
/// changed so the caller can persist it. (#new-user-defaults)
fn migrate_defaults(cfg: &mut ConfigFile) -> bool {
    if cfg.defaults_rev >= DEFAULTS_REV {
        return false;
    }
    // rev 1: miku / welcome-as-sidebar / right-docked resources / wallpaper overlay.
    if cfg.defaults_rev < 1 {
        // Old default wallpaper → miku. A custom path, "none" (""), or any other
        // built-in means the user chose it, so leave it.
        if cfg.wallpaper == "builtin:tech" {
            cfg.wallpaper = "builtin:miku".to_string();
        }
        // Overlay still unset -> current default.
        if cfg.wallpaper_overlay <= 0.0 {
            cfg.wallpaper_overlay = DEFAULT_WALLPAPER_OVERLAY;
        }
        // Never enabled the welcome sidebar → enable it.
        if !cfg.welcome_as_sidebar {
            cfg.welcome_as_sidebar = true;
        }
        // Never moved the resource panel (empty = the old left default) → right.
        if cfg.sidebar_dock.trim().is_empty() {
            cfg.sidebar_dock = "right".to_string();
        }
    }
    // rev 2: settings show wallpaper transparency, while rev 1 accidentally
    // stored the default as panel alpha 0.38, so it displayed as ~62%.
    if cfg.defaults_rev < 2
        && (cfg.wallpaper_overlay - PREVIOUS_DEFAULT_WALLPAPER_TRANSPARENCY).abs() < 0.005
    {
        cfg.wallpaper_overlay = DEFAULT_WALLPAPER_OVERLAY;
    }
    // rev 3: reduce the default transparency from 38% to 15%. Only advance
    // users still on the previous default; preserve every custom slider value.
    if cfg.defaults_rev < 3
        && (cfg.wallpaper_overlay - PREVIOUS_DEFAULT_WALLPAPER_OVERLAY).abs() < 0.005
    {
        cfg.wallpaper_overlay = DEFAULT_WALLPAPER_OVERLAY;
    }
    cfg.defaults_rev = DEFAULTS_REV;
    true
}

fn normalize_highlight_color(color: &str) -> &'static str {
    match color {
        "yellow" => "yellow",
        "green" => "green",
        "cyan" => "cyan",
        "magenta" => "magenta",
        "gray" => "gray",
        _ => "red",
    }
}

/// Remove duplicate entries in place, keeping the *last* (most recent)
/// occurrence of each and preserving relative order (#113). The list is capped
/// at 200, so the quadratic scan is trivial.
fn dedup_keep_last(items: &mut Vec<String>) {
    let mut i = 0;
    while i < items.len() {
        if items[i + 1..].contains(&items[i]) {
            items.remove(i);
        } else {
            i += 1;
        }
    }
}

/// Display-only session groups that must never be persisted as user folders.
/// `default` maps to an empty group; `system` is owned by built-in local shells.
pub(crate) fn is_reserved_session_group(name: &str) -> bool {
    name.eq_ignore_ascii_case("default") || name.eq_ignore_ascii_case("system")
}

/// Named display groups: explicit folders ∪ the groups sessions are filed
/// under, with reserved names and ungrouped excluded, de-duplicated and
/// sorted case-insensitively. The single source of truth for group display
/// order — shared by the welcome list, drag-reorder target finding and the
/// group dropdown; if these ever drift, drop targets and rendered rows
/// disagree (#41).
pub(crate) fn named_display_groups(explicit: &[String], sessions: &[Session]) -> Vec<String> {
    let mut named: Vec<String> = explicit
        .iter()
        .filter(|group| !is_reserved_session_group(group.trim()))
        .cloned()
        .chain(
            sessions
                .iter()
                .filter(|session| {
                    !session.group.is_empty() && !is_reserved_session_group(session.group.trim())
                })
                .map(|session| session.group.clone()),
        )
        .collect();
    named.sort_by_key(|group| group.to_lowercase());
    named.dedup();
    named
}

/// Repair configurations created before #316/#324, when the Move-to menu exposed
/// the built-in `system` group as a destination for saved server sessions.
fn normalize_reserved_session_groups(cfg: &mut ConfigFile) -> bool {
    let old_group_count = cfg.groups.len();
    cfg.groups
        .retain(|group| !is_reserved_session_group(group.trim()));
    let mut changed = cfg.groups.len() != old_group_count;
    for session in &mut cfg.sessions {
        if is_reserved_session_group(session.group.trim()) {
            session.group.clear();
            changed = true;
        }
    }
    changed
}

#[cfg(any(target_os = "macos", test))]
fn normalize_macos_renderer_mode(mode: &str) -> &'static str {
    match mode {
        "femtovg" => "femtovg",
        "skia" => "skia",
        _ => "software",
    }
}

impl ConfigStore {
    /// The prefix that marks an encrypted password blob in sessions.json.
    const ENC_PREFIX: &'static str = "enc:v1:";

    /// Marks a password encrypted with the **portable export key** (issue #46).
    const EXPORT_PREFIX: &'static str = "enc:exp:v1:";

    /// Fixed 32-byte key for portable exports. Baked into the binary so an
    /// exported file decrypts on any machine. Obfuscation only — see `ExportFile`.
    const EXPORT_KEY: [u8; 32] = *b"meatshell.export.portable.key.01";

    // ── Encryption helpers ────────────────────────────────────────────────

    /// Encrypt `plaintext` with ChaCha20-Poly1305 and return
    /// `"enc:v1:<base64url(nonce_12_bytes || ciphertext)>"`.
    fn encrypt(key: &[u8; 32], plaintext: &str) -> Result<String> {
        let cipher = ChaCha20Poly1305::new(key.into());
        let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng); // 12 random bytes
        let ciphertext = cipher
            .encrypt(&nonce, plaintext.as_bytes())
            .map_err(|e| anyhow::anyhow!("password encrypt error: {e}"))?;
        let mut blob = nonce.to_vec();
        blob.extend_from_slice(&ciphertext);
        Ok(format!(
            "{}{}",
            Self::ENC_PREFIX,
            URL_SAFE_NO_PAD.encode(&blob)
        ))
    }

    /// Try to decrypt a value produced by [`Self::encrypt`].
    /// Returns `None` if the string is not an encrypted blob (e.g. a legacy
    /// plaintext value, an empty string, or a tampered/corrupt blob).
    fn try_decrypt(key: &[u8; 32], s: &str) -> Option<String> {
        let b64 = s.strip_prefix(Self::ENC_PREFIX)?;
        let blob = URL_SAFE_NO_PAD.decode(b64).ok()?;
        if blob.len() < 12 {
            return None;
        }
        let (nonce_bytes, ciphertext) = blob.split_at(12);
        let cipher = ChaCha20Poly1305::new(key.into());
        let nonce = chacha20poly1305::Nonce::from_slice(nonce_bytes);
        let plain = cipher.decrypt(nonce, ciphertext).ok()?;
        String::from_utf8(plain).ok()
    }

    // ── Key file management ───────────────────────────────────────────────

    /// Load the 32-byte key from `<config_dir>/secret.key`, or generate and
    /// persist a fresh one.  On Unix the key file is created with mode `0600`
    /// so other local accounts cannot read it.  On Windows files in `%APPDATA%`
    /// are already restricted to the owning user by default ACLs.
    fn load_or_create_key(config_dir: &Path) -> Result<[u8; 32]> {
        use rand::RngCore as _;
        let key_path = config_dir.join("secret.key");

        if key_path.exists() {
            let bytes = fs::read(&key_path)
                .with_context(|| format!("failed to read {}", key_path.display()))?;
            if bytes.len() == 32 {
                let mut key = [0u8; 32];
                key.copy_from_slice(&bytes);
                return Ok(key);
            }
            tracing::warn!("secret.key has wrong length — regenerating");
        }

        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);
        fs::write(&key_path, &key)
            .with_context(|| format!("failed to write {}", key_path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))
                .with_context(|| format!("failed to set permissions on {}", key_path.display()))?;
        }
        tracing::info!("generated new encryption key at {}", key_path.display());
        Ok(key)
    }

    // ── Public API ────────────────────────────────────────────────────────

    /// Load (or initialise) the config file. On any parse error we back up the
    /// broken file and start fresh — losing saved sessions is better than
    /// crashing at launch.
    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        let config_dir = path
            .parent()
            .context("config path has no parent directory")?
            .to_path_buf();

        fs::create_dir_all(&config_dir)
            .with_context(|| format!("failed to create config dir {}", config_dir.display()))?;

        let backup_dir = legacy_data_dir().filter(|dir| dir != &config_dir);
        if let Some(ref backup) = backup_dir {
            restore_user_backup_if_needed(&config_dir, backup);
        }

        let key = Self::load_or_create_key(&config_dir)?;

        let mut migrated = false;
        let cache = if path.exists() {
            let raw = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            match serde_json::from_str::<ConfigFile>(&raw) {
                Ok(mut cfg) => {
                    // Decrypt any encrypted passwords; leave legacy plaintext
                    // values untouched (they will be encrypted on next save).
                    for session in &mut cfg.sessions {
                        if let Some(plain) = Self::try_decrypt(&key, session.password.as_str()) {
                            session.password = Secret::new(plain);
                        }
                        if let Some(plain) =
                            Self::try_decrypt(&key, session.private_key_inline.as_str())
                        {
                            session.private_key_inline = Secret::new(plain);
                        }
                    }
                    if let Some(plain) = Self::try_decrypt(&key, cfg.webdav_password.as_str()) {
                        cfg.webdav_password = Secret::new(plain);
                    }
                    // Clean up any duplicate history accumulated before #113,
                    // keeping the last (most recent) occurrence of each command.
                    dedup_keep_last(&mut cfg.command_history);
                    // `system` and `default` are display-only group names. Older
                    // builds allowed moving saved servers into `system`, creating
                    // a duplicate empty-menu folder (#316, #324).
                    migrated |= normalize_reserved_session_groups(&mut cfg);
                    // One-time push of the new default layout to existing users
                    // (only for items they never changed). (#new-user-defaults)
                    migrated |= migrate_defaults(&mut cfg);
                    cfg
                }
                Err(err) => {
                    let backup = path.with_extension("json.broken");
                    let _ = fs::rename(&path, &backup);
                    tracing::warn!(
                        "config file was corrupt ({err}); backed up to {}",
                        backup.display()
                    );
                    fresh_config()
                }
            }
        } else {
            fresh_config()
        };

        let store = Self {
            path,
            backup_dir,
            cache,
            key,
        };
        // Persist the migration so it runs exactly once (and so a later opt-out —
        // e.g. turning the welcome sidebar back off — isn't reverted next launch).
        if migrated {
            if let Err(e) = store.save() {
                tracing::warn!("failed to persist default-layout migration: {e:#}");
            }
        }
        Ok(store)
    }

    fn config_path() -> Result<PathBuf> {
        Ok(data_dir().join("sessions.json"))
    }

    pub fn sessions(&self) -> &[Session] {
        &self.cache.sessions
    }

    /// Drag-to-reorder a saved session (`dir < 0` = up). The stored Vec order
    /// is the display order within a group (same convention as quick commands).
    /// Same-group hops swap neighbours; when there is no same-group neighbour
    /// the hop crosses the group boundary instead: the session moves into the
    /// nearest visible display group along `dir`, landing at that group's
    /// boundary (first member moving down, last moving up). Returns whether
    /// anything changed.
    pub fn reorder_session(&mut self, id: &str, dir: isize) -> bool {
        // Display group of a session: ungrouped (and reserved names) render
        // under "default"; everything else under its own group. Mirrors
        // build_session_rows in src/app/session_models.rs.
        fn display_group(session: &Session) -> String {
            if session.group.is_empty() || is_reserved_session_group(session.group.trim()) {
                "default".to_string()
            } else {
                session.group.clone()
            }
        }

        let Some(idx) = self.cache.sessions.iter().position(|s| s.id == id) else {
            return false;
        };
        let group = display_group(&self.cache.sessions[idx]);

        // Same-group neighbour in stored (= display) order → plain swap.
        let same_group_target = {
            let sessions = &self.cache.sessions;
            if dir < 0 {
                (0..idx)
                    .rev()
                    .find(|&i| display_group(&sessions[i]) == group)
            } else {
                (idx + 1..sessions.len()).find(|&i| display_group(&sessions[i]) == group)
            }
        };
        if let Some(target) = same_group_target {
            self.cache.sessions.swap(idx, target);
            return true;
        }

        // Cross-group hop. Display order: "default" first (only when ungrouped
        // sessions exist), then named groups — explicit folders ∪ sessions'
        // groups — alphabetically. Collapsed groups are skipped: their rows
        // are hidden, so a card must never land inside one.
        let Some(collapsed_groups) = self.cache.collapsed_session_groups.clone() else {
            // Mirrors build_session_rows: no collapse list = everything
            // collapsed, so no visible target group can exist.
            return false;
        };
        let is_collapsed = |name: &str| collapsed_groups.iter().any(|g| g == name);

        let mut display: Vec<String> = Vec::new();
        if self
            .cache
            .sessions
            .iter()
            .any(|s| display_group(s) == "default")
        {
            display.push("default".to_string());
        }
        display.extend(named_display_groups(
            &self.cache.groups,
            &self.cache.sessions,
        ));

        let Some(pos) = display.iter().position(|g| g == &group) else {
            return false;
        };
        let target_group = if dir < 0 {
            (0..pos).rev().find(|&i| !is_collapsed(&display[i]))
        } else {
            (pos + 1..display.len()).find(|&i| !is_collapsed(&display[i]))
        }
        .map(|i| display[i].clone());
        let Some(target_group) = target_group else {
            return false;
        };

        let source_group = self.cache.sessions[idx].group.clone();
        let mut moved = self.cache.sessions.remove(idx);
        moved.group = if target_group == "default" {
            String::new()
        } else {
            target_group.clone()
        };
        let insert_at = {
            let members: Vec<usize> = self
                .cache
                .sessions
                .iter()
                .enumerate()
                .filter(|(_, s)| display_group(s) == target_group)
                .map(|(i, _)| i)
                .collect();
            if members.is_empty() {
                idx.min(self.cache.sessions.len())
            } else if dir < 0 {
                members[members.len() - 1] + 1
            } else {
                members[0]
            }
        };
        self.cache.sessions.insert(insert_at, moved);

        // A named source group that just lost its last member would vanish
        // from the list (changing the row count mid-drag, which drops the
        // dragging row's pointer grab); keep it as an empty folder. Register
        // it in `groups` directly rather than via add_group: the folder was
        // visibly expanded during the drag, and add_group would collapse it.
        if group != "default"
            && !self.cache.sessions.iter().any(|s| s.group == source_group)
            && !self.cache.groups.iter().any(|g| g == &source_group)
        {
            self.cache.groups.push(source_group);
        }
        true
    }

    pub fn upsert(&mut self, mut session: Session) {
        if is_reserved_session_group(session.group.trim()) {
            session.group.clear();
        }
        if let Some(existing) = self.cache.sessions.iter_mut().find(|s| s.id == session.id) {
            *existing = session;
        } else {
            self.cache.sessions.push(session);
        }
    }

    pub fn remove(&mut self, id: &str) {
        self.cache.sessions.retain(|s| s.id != id);
    }

    pub fn get(&self, id: &str) -> Option<&Session> {
        self.cache.sessions.iter().find(|s| s.id == id)
    }

    pub fn download_dir(&self) -> &str {
        &self.cache.download_dir
    }

    pub fn set_download_dir(&mut self, dir: String) {
        self.cache.download_dir = dir;
    }

    /// UI language code ("zh" default / "en").
    pub fn language(&self) -> &str {
        if self.cache.language.is_empty() {
            "zh"
        } else {
            &self.cache.language
        }
    }

    pub fn set_language(&mut self, lang: String) {
        self.cache.language = lang;
    }

    /// Theme preference: "system" (default) | "dark" | "light".
    pub fn theme_pref(&self) -> &str {
        if self.cache.theme_pref.is_empty() {
            "system"
        } else {
            &self.cache.theme_pref
        }
    }

    pub fn set_theme_pref(&mut self, pref: String) {
        self.cache.theme_pref = pref;
    }

    /// Renderer preference for the current platform.
    #[cfg(target_os = "macos")]
    pub fn renderer_mode(&self) -> &str {
        normalize_macos_renderer_mode(&self.cache.renderer_mode)
    }

    /// Missing and invalid Windows values deliberately use software so upgrades
    /// preserve the high-DPI/VM compatibility from #224.
    #[cfg(target_os = "windows")]
    pub fn renderer_mode(&self) -> &str {
        match self.cache.renderer_mode.as_str() {
            "auto" => "auto",
            "gpu" => "gpu",
            _ => "software",
        }
    }

    #[cfg(target_os = "macos")]
    pub fn set_renderer_mode(&mut self, mode: String) {
        self.cache.renderer_mode = normalize_macos_renderer_mode(&mode).into();
    }

    /// Linux previously used Slint's automatic renderer selection and had no
    /// settings entry. Keep that behaviour for existing configurations.
    #[cfg(target_os = "linux")]
    pub fn renderer_mode(&self) -> &str {
        match self.cache.renderer_mode.as_str() {
            "gpu" => "gpu",
            "software" => "software",
            _ => "auto",
        }
    }

    #[cfg(target_os = "windows")]
    pub fn set_renderer_mode(&mut self, mode: String) {
        self.cache.renderer_mode = match mode.as_str() {
            "auto" => "auto".into(),
            "gpu" => "gpu".into(),
            _ => "software".into(),
        };
    }

    #[cfg(target_os = "linux")]
    pub fn set_renderer_mode(&mut self, mode: String) {
        self.cache.renderer_mode = match mode.as_str() {
            "gpu" => "gpu".into(),
            "software" => "software".into(),
            _ => "auto".into(),
        };
    }

    /// Terminal font family ("" = built-in default).
    pub fn font_family(&self) -> &str {
        &self.cache.font_family
    }

    pub fn set_font_family(&mut self, family: String) {
        self.cache.font_family = family;
    }

    /// Terminal font size in px (falls back to 13 when unset).
    pub fn font_size(&self) -> u32 {
        if self.cache.font_size == 0 {
            13
        } else {
            self.cache.font_size
        }
    }

    pub fn set_font_size(&mut self, size: u32) {
        self.cache.font_size = size.clamp(8, 32);
    }

    pub fn terminal_line_spacing(&self) -> f32 {
        let value = self.cache.terminal_line_spacing;
        if value <= 0.0 {
            1.0
        } else {
            value.clamp(0.8, 1.5)
        }
    }

    pub fn set_terminal_line_spacing(&mut self, value: f32) {
        self.cache.terminal_line_spacing = value.clamp(0.8, 1.5);
    }

    pub fn paste_confirm_enabled(&self) -> bool {
        !self.cache.paste_confirm_disabled
    }

    pub fn set_paste_confirm_enabled(&mut self, enabled: bool) {
        self.cache.paste_confirm_disabled = !enabled;
    }

    pub fn extra_paste_shortcuts_enabled(&self) -> bool {
        !self.cache.extra_paste_shortcuts_disabled
    }

    pub fn set_extra_paste_shortcuts_enabled(&mut self, enabled: bool) {
        self.cache.extra_paste_shortcuts_disabled = !enabled;
    }

    pub fn zen_mode(&self) -> bool {
        self.cache.zen_mode
    }

    pub fn set_zen_mode(&mut self, enabled: bool) {
        self.cache.zen_mode = enabled;
    }

    /// Force regular terminal text to render with a bold face (#262).
    pub fn terminal_bold(&self) -> bool {
        self.cache.terminal_bold
    }

    pub fn set_terminal_bold(&mut self, bold: bool) {
        self.cache.terminal_bold = bold;
    }

    /// Selected terminal insertion cursor shape. Legacy and invalid values use
    /// the existing block cursor so upgrades preserve the current appearance.
    pub fn terminal_cursor_style(&self) -> &str {
        match self.cache.terminal_cursor_style.as_str() {
            "bar" => "bar",
            "underline" => "underline",
            _ => "block",
        }
    }

    pub fn set_terminal_cursor_style(&mut self, style: String) {
        self.cache.terminal_cursor_style = match style.as_str() {
            "bar" => "bar".into(),
            "underline" => "underline".into(),
            _ => "block".into(),
        };
    }

    pub fn terminal_cursor_color(&self) -> &str {
        if normalize_hex_color(&self.cache.terminal_cursor_color).is_some() {
            &self.cache.terminal_cursor_color
        } else {
            ""
        }
    }

    pub fn set_terminal_cursor_color(&mut self, color: &str) -> bool {
        let Some(normalized) = normalize_hex_color(color) else {
            return false;
        };
        self.cache.terminal_cursor_color = normalized;
        true
    }

    /// Whether client-side highlighting of otherwise unstyled output is active.
    pub fn output_highlight_enabled(&self) -> bool {
        !self.cache.output_highlight_disabled
    }

    pub fn set_output_highlight_enabled(&mut self, enabled: bool) {
        self.cache.output_highlight_disabled = !enabled;
    }

    pub fn json_format_output(&self) -> bool {
        !self.cache.json_format_disabled
    }

    pub fn set_json_format_output(&mut self, enabled: bool) {
        self.cache.json_format_disabled = !enabled;
    }

    /// Selected built-in rule set. Unknown values safely fall back to the
    /// conservative log-level preset for forward/backward compatibility.
    pub fn output_highlight_preset(&self) -> &str {
        match self.cache.output_highlight_preset.as_str() {
            "devops" => "devops",
            _ => "log",
        }
    }

    pub fn set_output_highlight_preset(&mut self, preset: String) {
        self.cache.output_highlight_preset = match preset.as_str() {
            "devops" => "devops".to_string(),
            _ => "log".to_string(),
        };
    }

    pub fn output_highlight_rules(&self) -> &[OutputHighlightRule] {
        &self.cache.output_highlight_rules
    }

    pub fn add_output_highlight_rule(&mut self, mut rule: OutputHighlightRule) {
        rule.pattern = rule.pattern.trim().to_string();
        rule.color = normalize_highlight_color(&rule.color).to_string();
        self.cache.output_highlight_rules.push(rule);
    }

    pub fn remove_output_highlight_rule(&mut self, index: usize) {
        if index < self.cache.output_highlight_rules.len() {
            self.cache.output_highlight_rules.remove(index);
        }
    }

    pub fn set_output_highlight_rule_enabled(&mut self, index: usize, enabled: bool) {
        if let Some(rule) = self.cache.output_highlight_rules.get_mut(index) {
            rule.enabled = enabled;
        }
    }

    /// Global UI scale in percent (#100). Defaults to 100.
    pub fn ui_scale(&self) -> u32 {
        if self.cache.ui_scale == 0 {
            100
        } else {
            self.cache.ui_scale
        }
    }

    pub fn set_ui_scale(&mut self, percent: u32) {
        self.cache.ui_scale = percent.clamp(80, 200);
    }

    /// Immersive wallpaper id ("" = none).
    pub fn wallpaper(&self) -> &str {
        &self.cache.wallpaper
    }

    pub fn set_wallpaper(&mut self, id: impl Into<String>) {
        self.cache.wallpaper = id.into();
    }

    /// Whether the SFTP panel follows the terminal's cd (default true).
    pub fn sftp_follow_cd(&self) -> bool {
        !self.cache.sftp_no_follow_cd
    }

    pub fn set_sftp_follow_cd(&mut self, follow: bool) {
        self.cache.sftp_no_follow_cd = !follow;
    }

    /// Whether the quick-command bar under the terminal is hidden.
    pub fn cmd_bar_hidden(&self) -> bool {
        self.cache.hide_cmd_bar
    }

    pub fn set_cmd_bar_hidden(&mut self, hidden: bool) {
        self.cache.hide_cmd_bar = hidden;
    }

    /// Saved quick commands (#55).
    pub fn quick_commands(&self) -> &[QuickCommand] {
        &self.cache.quick_commands
    }

    pub fn set_quick_commands(&mut self, cmds: Vec<QuickCommand>) {
        self.cache.quick_commands = cmds;
    }

    pub fn wsl_profiles(&self) -> &[WslProfile] {
        &self.cache.wsl_profiles
    }

    pub fn add_wsl_profile(&mut self, name: String, distribution: String, directory: String) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        self.cache.wsl_profiles.push(WslProfile {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            distribution: distribution.trim().to_string(),
            directory: match directory.trim() {
                "" => "~".to_string(),
                value => value.to_string(),
            },
        });
    }

    pub fn remove_wsl_profile(&mut self, id: &str) {
        self.cache.wsl_profiles.retain(|profile| profile.id != id);
    }

    pub fn quick_panel_open(&self) -> bool {
        self.cache.quick_panel_open
    }

    pub fn quick_commands_as_sidebar(&self) -> bool {
        self.cache.quick_commands_as_sidebar
    }

    pub fn set_quick_commands_as_sidebar(&mut self, enabled: bool) {
        self.cache.quick_commands_as_sidebar = enabled;
        if !enabled {
            self.cache.quick_panel_open = false;
        }
    }

    pub fn set_quick_panel_open(&mut self, open: bool) {
        self.cache.quick_panel_open = open;
    }

    pub fn quick_panel_collapsed(&self) -> bool {
        self.cache.quick_panel_collapsed
    }

    pub fn set_quick_panel_collapsed(&mut self, collapsed: bool) {
        self.cache.quick_panel_collapsed = collapsed;
    }

    pub fn quick_panel_width(&self) -> f32 {
        let width = self.cache.quick_panel_width;
        if width <= 0.0 {
            default_quick_panel_width()
        } else {
            width
        }
    }

    pub fn set_quick_panel_width(&mut self, width: f32) {
        self.cache.quick_panel_width = width;
    }

    pub fn quick_panel_height(&self) -> f32 {
        let height = self.cache.quick_panel_height;
        if height <= 0.0 {
            default_quick_panel_height()
        } else {
            height
        }
    }

    pub fn set_quick_panel_height(&mut self, height: f32) {
        self.cache.quick_panel_height = height;
    }

    pub fn quick_panel_dock(&self) -> String {
        match self.cache.quick_panel_dock.trim() {
            "left" | "right" | "top" | "bottom" => self.cache.quick_panel_dock.clone(),
            _ => "right".into(),
        }
    }

    pub fn set_quick_panel_dock(&mut self, dock: String) {
        self.cache.quick_panel_dock = dock;
    }

    /// Explicit quick-command groups (#55) — parallels [`groups`](Self::groups).
    pub fn quick_groups(&self) -> &[String] {
        &self.cache.quick_groups
    }

    /// Create an empty quick-command group. Ignores blank, "default", duplicates.
    pub fn add_quick_group(&mut self, name: String) {
        let n = name.trim().to_string();
        if n.is_empty() || n.eq_ignore_ascii_case("default") {
            return;
        }
        if !self.cache.quick_groups.iter().any(|g| g == &n) {
            self.cache.quick_groups.push(n);
        }
    }

    /// Delete a quick-command group; any command still in it falls back to
    /// ungrouped (the UI only offers delete on empty groups, but clear defensively).
    pub fn remove_quick_group(&mut self, name: &str) {
        self.cache.quick_groups.retain(|g| g != name);
        for c in &mut self.cache.quick_commands {
            if c.group == name {
                c.group.clear();
            }
        }
    }

    /// Rename a quick-command group, moving its commands along. No-op for
    /// blank / "default".
    pub fn rename_quick_group(&mut self, old: &str, new: String) {
        let n = new.trim().to_string();
        if n.is_empty() || n.eq_ignore_ascii_case("default") || n == old {
            return;
        }
        for g in &mut self.cache.quick_groups {
            if g == old {
                *g = n.clone();
            }
        }
        for c in &mut self.cache.quick_commands {
            if c.group == old {
                c.group = n.clone();
            }
        }
        self.cache.quick_groups.sort();
        self.cache.quick_groups.dedup();
    }

    /// Update one quick command in place by index (#55).
    pub fn update_quick_command(&mut self, index: usize, cmd: QuickCommand) {
        if let Some(slot) = self.cache.quick_commands.get_mut(index) {
            *slot = cmd;
        }
    }

    /// Recent command-box history, oldest first (#55).
    pub fn command_history(&self) -> &[String] {
        &self.cache.command_history
    }

    /// Append a command to the history: skips blanks, de-duplicates globally so
    /// each command appears once, and re-appends at the end so the most-recently
    /// used command is always last. Capped so it can't grow without bound (#113).
    pub fn push_command_history(&mut self, cmd: String) {
        if cmd.trim().is_empty() {
            return;
        }
        // Drop any earlier occurrence, then push → no duplicates and "last used"
        // moves to the end (bash `HISTCONTROL=erasedups` semantics).
        self.cache.command_history.retain(|c| c != &cmd);
        const CAP: usize = 200;
        self.cache.command_history.push(cmd);
        let len = self.cache.command_history.len();
        if len > CAP {
            self.cache.command_history.drain(0..len - CAP);
        }
    }

    /// Remove a single command-history entry by storage index (#96).
    pub fn remove_command_history(&mut self, index: usize) {
        if index < self.cache.command_history.len() {
            self.cache.command_history.remove(index);
        }
    }

    /// Collapse the resource sidebar on startup (default false) (#78).
    pub fn collapse_sidebar_default(&self) -> bool {
        self.cache.collapse_sidebar_default
    }

    pub fn set_collapse_sidebar_default(&mut self, v: bool) {
        self.cache.collapse_sidebar_default = v;
    }

    /// Persisted sidebar width in logical px. Falls back to the default when the
    /// stored value is unset/zero (e.g. a config created via `Default`).
    pub fn sidebar_width(&self) -> f32 {
        let w = self.cache.sidebar_width;
        if w <= 0.0 {
            default_sidebar_width()
        } else {
            w
        }
    }

    pub fn set_sidebar_width(&mut self, v: f32) {
        self.cache.sidebar_width = v;
    }

    /// Resource / SFTP panel docking geometry, persisted across restarts (#dock).
    /// Sizes fall back to their defaults when unset/zero; docks fall back to a
    /// sensible edge when the stored string is empty.
    pub fn sidebar_height(&self) -> f32 {
        let h = self.cache.sidebar_height;
        if h <= 0.0 {
            default_sidebar_height()
        } else {
            h
        }
    }
    pub fn set_sidebar_height(&mut self, v: f32) {
        self.cache.sidebar_height = v;
    }
    pub fn sidebar_dock(&self) -> String {
        let d = self.cache.sidebar_dock.trim();
        if d.is_empty() {
            "left".into()
        } else {
            d.to_string()
        }
    }
    pub fn set_sidebar_dock(&mut self, v: String) {
        self.cache.sidebar_dock = v;
    }
    pub fn sidebar_collapsed(&self) -> Option<bool> {
        self.cache.sidebar_collapsed
    }
    pub fn set_sidebar_collapsed(&mut self, v: bool) {
        self.cache.sidebar_collapsed = Some(v);
    }
    pub fn welcome_as_sidebar(&self) -> bool {
        self.cache.welcome_as_sidebar
    }
    pub fn set_welcome_as_sidebar(&mut self, v: bool) {
        self.cache.welcome_as_sidebar = v;
    }
    pub fn welcome_sidebar_width(&self) -> f32 {
        let w = self.cache.welcome_sidebar_width;
        if w <= 0.0 {
            240.0
        } else {
            w
        }
    }
    pub fn set_welcome_sidebar_width(&mut self, v: f32) {
        self.cache.welcome_sidebar_width = v;
    }
    pub fn welcome_sidebar_dock(&self) -> String {
        let d = self.cache.welcome_sidebar_dock.trim();
        if d.is_empty() {
            "left".into()
        } else {
            d.to_string()
        }
    }
    pub fn set_welcome_sidebar_dock(&mut self, v: String) {
        self.cache.welcome_sidebar_dock = v;
    }
    pub fn welcome_collapsed(&self) -> Option<bool> {
        self.cache.welcome_collapsed
    }
    pub fn set_welcome_collapsed(&mut self, v: bool) {
        self.cache.welcome_collapsed = Some(v);
    }
    /// Whether the startup new-version check is enabled (#184).
    pub fn update_check_enabled(&self) -> bool {
        !self.cache.update_check_disabled
    }
    pub fn set_update_check_enabled(&mut self, enabled: bool) {
        self.cache.update_check_disabled = !enabled;
    }
    pub fn mcp_enabled(&self) -> bool {
        self.cache.mcp_enabled
    }
    pub fn set_mcp_enabled(&mut self, enabled: bool) {
        self.cache.mcp_enabled = enabled;
    }
    pub fn mcp_use_saved_credentials(&self) -> bool {
        self.cache.mcp_use_saved_credentials
    }
    pub fn set_mcp_use_saved_credentials(&mut self, enabled: bool) {
        self.cache.mcp_use_saved_credentials = enabled;
    }
    pub fn mcp_allow_commands(&self) -> bool {
        self.cache.mcp_allow_commands
    }
    pub fn set_mcp_allow_commands(&mut self, enabled: bool) {
        self.cache.mcp_allow_commands = enabled;
    }
    pub fn mcp_allow_file_transfers(&self) -> bool {
        self.cache.mcp_allow_file_transfers
    }
    pub fn set_mcp_allow_file_transfers(&mut self, enabled: bool) {
        self.cache.mcp_allow_file_transfers = enabled;
    }
    pub fn wallpaper_overlay(&self) -> f32 {
        let a = self.cache.wallpaper_overlay;
        // Floor lowered 0.40 -> 0.30 so more see-through panels are reachable.
        if a <= 0.0 {
            DEFAULT_WALLPAPER_OVERLAY
        } else {
            a.clamp(0.30, 1.0)
        }
    }
    pub fn set_wallpaper_overlay(&mut self, v: f32) {
        self.cache.wallpaper_overlay = v.clamp(0.30, 1.0);
    }
    pub fn panel_font(&self) -> u32 {
        if self.cache.panel_font == 0 {
            100
        } else {
            self.cache.panel_font
        }
    }
    pub fn set_panel_font(&mut self, percent: u32) {
        self.cache.panel_font = percent.clamp(80, 160);
    }
    pub fn sftp_panel_width(&self) -> f32 {
        let w = self.cache.sftp_panel_width;
        if w <= 0.0 {
            default_sftp_width()
        } else {
            w
        }
    }
    pub fn set_sftp_panel_width(&mut self, v: f32) {
        self.cache.sftp_panel_width = v;
    }
    pub fn sftp_panel_height(&self) -> f32 {
        let h = self.cache.sftp_panel_height;
        if h <= 0.0 {
            default_sftp_height()
        } else {
            h
        }
    }
    pub fn set_sftp_panel_height(&mut self, v: f32) {
        self.cache.sftp_panel_height = v;
    }
    pub fn sftp_tree_width(&self) -> f32 {
        let width = self.cache.sftp_tree_width;
        if width <= 0.0 {
            default_sftp_tree_width()
        } else {
            width.clamp(120.0, 420.0)
        }
    }
    pub fn set_sftp_tree_width(&mut self, width: f32) {
        self.cache.sftp_tree_width = width.clamp(120.0, 420.0);
    }
    pub fn sftp_dock(&self) -> String {
        let d = self.cache.sftp_dock.trim();
        if d.is_empty() {
            "bottom".into()
        } else {
            d.to_string()
        }
    }
    pub fn set_sftp_dock(&mut self, v: String) {
        self.cache.sftp_dock = v;
    }
    /// Last window size in logical px; `(0,0)` means unset (use the default).
    pub fn window_size(&self) -> (f32, f32) {
        (self.cache.window_width, self.cache.window_height)
    }
    pub fn set_window_size(&mut self, w: f32, h: f32) {
        self.cache.window_width = w;
        self.cache.window_height = h;
    }

    /// Collapse the SFTP panel on startup (default false) (#78).
    pub fn collapse_sftp_default(&self) -> bool {
        self.cache.collapse_sftp_default
    }

    pub fn set_collapse_sftp_default(&mut self, v: bool) {
        self.cache.collapse_sftp_default = v;
    }

    /// Mirror SFTP uploads to other sessions while session-sync is on (default
    /// false). Only has effect when the session-sync toggle is on.
    pub fn sync_upload(&self) -> bool {
        self.cache.sync_upload
    }

    pub fn set_sync_upload(&mut self, v: bool) {
        self.cache.sync_upload = v;
    }

    pub fn webdav_enabled(&self) -> bool {
        self.cache.webdav_enabled
    }

    pub fn webdav_url(&self) -> &str {
        &self.cache.webdav_url
    }

    pub fn webdav_username(&self) -> &str {
        &self.cache.webdav_username
    }

    pub fn webdav_password(&self) -> &str {
        self.cache.webdav_password.as_str()
    }

    pub fn webdav_remote_path(&self) -> &str {
        if self.cache.webdav_remote_path.trim().is_empty() {
            "meatshell-connections.json"
        } else {
            &self.cache.webdav_remote_path
        }
    }

    pub fn webdav_accept_invalid_certs(&self) -> bool {
        self.cache.webdav_accept_invalid_certs
    }

    pub fn set_webdav_settings(
        &mut self,
        enabled: bool,
        url: String,
        username: String,
        password: String,
        remote_path: String,
        accept_invalid_certs: bool,
    ) {
        self.cache.webdav_enabled = enabled;
        self.cache.webdav_url = url.trim().trim_end_matches('/').to_string();
        self.cache.webdav_username = username.trim().to_string();
        self.cache.webdav_password = Secret::new(password);
        self.cache.webdav_remote_path = if remote_path.trim().is_empty() {
            "meatshell-connections.json".to_string()
        } else {
            remote_path.trim().trim_start_matches('/').to_string()
        };
        self.cache.webdav_accept_invalid_certs = accept_invalid_certs;
    }

    /// Whether each download prompts for a save location (default false) (#87).
    pub fn download_always_ask(&self) -> bool {
        self.cache.download_always_ask
    }

    pub fn set_download_always_ask(&mut self, ask: bool) {
        self.cache.download_always_ask = ask;
    }

    // ── Session groups / folders (#41) ────────────────────────────────────

    /// Explicit groups (empty folders included). "default" is implicit.
    pub fn groups(&self) -> &[String] {
        &self.cache.groups
    }

    pub fn collapsed_session_groups(&self) -> Option<&[String]> {
        self.cache.collapsed_session_groups.as_deref()
    }

    /// Remember a Quick Connect folder's open/closed state. On the first
    /// interaction, materialise the default-collapsed state for every existing
    /// folder so expanding one folder does not accidentally expand the rest.
    pub fn set_session_group_collapsed(&mut self, name: &str, collapsed: bool) {
        if self.cache.collapsed_session_groups.is_none() {
            let mut groups = vec!["system".to_string()];
            if self
                .cache
                .sessions
                .iter()
                .any(|session| session.group.is_empty())
            {
                groups.push("default".to_string());
            }
            groups.extend(self.cache.groups.iter().cloned());
            groups.extend(
                self.cache
                    .sessions
                    .iter()
                    .filter(|session| !session.group.is_empty())
                    .map(|session| session.group.clone()),
            );
            groups.sort();
            groups.dedup();
            self.cache.collapsed_session_groups = Some(groups);
        }

        let groups = self.cache.collapsed_session_groups.as_mut().unwrap();
        groups.retain(|group| group != name);
        if collapsed {
            groups.push(name.to_string());
            groups.sort();
            groups.dedup();
        }
    }

    /// Whether a user group already exists, including groups inferred from
    /// sessions that were created before explicit group records were added.
    pub fn session_group_exists(&self, name: &str) -> bool {
        let target = name.trim();
        if target.is_empty() {
            return false;
        }
        self.cache
            .groups
            .iter()
            .any(|group| group.trim().eq_ignore_ascii_case(target))
            || self.cache.sessions.iter().any(|session| {
                !session.group.trim().is_empty()
                    && session.group.trim().eq_ignore_ascii_case(target)
            })
    }

    /// Create an empty group. Ignores blank/reserved names and duplicates.
    pub fn add_group(&mut self, name: String) {
        let n = name.trim().to_string();
        if n.is_empty() || is_reserved_session_group(&n) || self.session_group_exists(&n) {
            return;
        }
        self.cache.groups.push(n.clone());
        if let Some(groups) = &mut self.cache.collapsed_session_groups {
            groups.push(n);
            groups.sort();
            groups.dedup();
        }
    }

    /// Delete a group. Any session still in it falls back to ungrouped — the UI
    /// only offers delete on empty groups, but we clear sessions defensively.
    pub fn remove_group(&mut self, name: &str) {
        if is_reserved_session_group(name.trim()) {
            return;
        }
        self.cache.groups.retain(|g| g != name);
        if let Some(groups) = &mut self.cache.collapsed_session_groups {
            groups.retain(|group| group != name);
        }
        for s in &mut self.cache.sessions {
            if s.group == name {
                s.group.clear();
            }
        }
    }

    /// Rename a group, moving its sessions along. No-op for reserved names.
    pub fn rename_group(&mut self, old: &str, new: String) {
        let n = new.trim().to_string();
        if n.is_empty()
            || is_reserved_session_group(old.trim())
            || is_reserved_session_group(&n)
            || n == old
            || (!n.eq_ignore_ascii_case(old) && self.session_group_exists(&n))
        {
            return;
        }
        for g in &mut self.cache.groups {
            if g == old {
                *g = n.clone();
            }
        }
        for s in &mut self.cache.sessions {
            if s.group == old {
                s.group = n.clone();
            }
        }
        if let Some(groups) = &mut self.cache.collapsed_session_groups {
            for group in groups.iter_mut() {
                if group == old {
                    *group = n.clone();
                }
            }
            groups.sort();
            groups.dedup();
        }
        self.cache.groups.sort();
        self.cache.groups.dedup();
    }

    pub fn save(&self) -> Result<()> {
        // Build a disk copy where every non-empty password is encrypted.
        let mut disk = self.cache.clone();
        for session in &mut disk.sessions {
            if !session.password.is_empty()
                && !session.password.as_str().starts_with(Self::ENC_PREFIX)
            {
                let enc = Self::encrypt(&self.key, session.password.as_str())?;
                session.password = Secret::new(enc);
            }
            if !session.private_key_inline.is_empty()
                && !session
                    .private_key_inline
                    .as_str()
                    .starts_with(Self::ENC_PREFIX)
            {
                let enc = Self::encrypt(&self.key, session.private_key_inline.as_str())?;
                session.private_key_inline = Secret::new(enc);
            }
        }
        if !disk.webdav_password.is_empty()
            && !disk.webdav_password.as_str().starts_with(Self::ENC_PREFIX)
        {
            let enc = Self::encrypt(&self.key, disk.webdav_password.as_str())?;
            disk.webdav_password = Secret::new(enc);
        }
        let raw = serde_json::to_string_pretty(&disk)?;
        // Write to a sibling temp file then rename — cheap atomicity.
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, &raw).with_context(|| format!("failed to write {}", tmp.display()))?;
        // Restrict to owner-only before publishing (#34): sessions.json holds
        // (encrypted) credentials, so it shouldn't be world-readable. Set 0600
        // on the temp file so the permission is already in place at rename.
        // Windows %APPDATA% is owner-restricted by default ACLs — no-op there.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))
                .with_context(|| format!("failed to set permissions on {}", tmp.display()))?;
        }
        fs::rename(&tmp, &self.path)
            .with_context(|| format!("failed to finalise {}", self.path.display()))?;
        self.sync_backup(&raw);
        Ok(())
    }

    fn sync_backup(&self, raw: &str) {
        let Some(backup_dir) = &self.backup_dir else {
            return;
        };
        if let Err(e) = fs::create_dir_all(backup_dir) {
            tracing::warn!(
                "failed to create user config backup dir {}: {e}",
                backup_dir.display()
            );
            return;
        }

        let backup_sessions = backup_dir.join("sessions.json");
        let tmp = backup_sessions.with_extension("json.tmp");
        if let Err(e) = fs::write(&tmp, raw) {
            tracing::warn!("failed to write {}: {e}", tmp.display());
            return;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
        }
        if let Err(e) = fs::rename(&tmp, &backup_sessions) {
            tracing::warn!("failed to finalise {}: {e}", backup_sessions.display());
        }

        if let Some(config_dir) = self.path.parent() {
            for name in ["secret.key", "known_hosts"] {
                let src = config_dir.join(name);
                let dst = backup_dir.join(name);
                if src.exists() {
                    if let Err(e) = fs::copy(&src, &dst) {
                        tracing::warn!(
                            "failed to sync {} to user config backup {}: {e}",
                            src.display(),
                            dst.display()
                        );
                    }
                    #[cfg(unix)]
                    if name == "secret.key" {
                        use std::os::unix::fs::PermissionsExt;
                        let _ = fs::set_permissions(&dst, fs::Permissions::from_mode(0o600));
                    }
                }
            }
        }
    }

    // ── Portable export / import (issue #46) ──────────────────────────────

    /// Encrypt a password with the portable export key → `"enc:exp:v1:<b64>"`.
    fn encrypt_export(plaintext: &str) -> Result<String> {
        let cipher = ChaCha20Poly1305::new((&Self::EXPORT_KEY).into());
        let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
        let ciphertext = cipher
            .encrypt(&nonce, plaintext.as_bytes())
            .map_err(|e| anyhow::anyhow!("export encrypt error: {e}"))?;
        let mut blob = nonce.to_vec();
        blob.extend_from_slice(&ciphertext);
        Ok(format!(
            "{}{}",
            Self::EXPORT_PREFIX,
            URL_SAFE_NO_PAD.encode(&blob)
        ))
    }

    /// Decrypt a value produced by [`Self::encrypt_export`]; `None` if it isn't one.
    fn decrypt_export(s: &str) -> Option<String> {
        let b64 = s.strip_prefix(Self::EXPORT_PREFIX)?;
        let blob = URL_SAFE_NO_PAD.decode(b64).ok()?;
        if blob.len() < 12 {
            return None;
        }
        let (nonce_bytes, ciphertext) = blob.split_at(12);
        let cipher = ChaCha20Poly1305::new((&Self::EXPORT_KEY).into());
        let nonce = chacha20poly1305::Nonce::from_slice(nonce_bytes);
        let plain = cipher.decrypt(nonce, ciphertext).ok()?;
        String::from_utf8(plain).ok()
    }

    /// Export all sessions to a portable JSON file. Passwords are re-encrypted
    /// with the built-in export key; everything else stays plaintext so the
    /// file is human-readable and editable. Returns the number of sessions.
    pub fn export_json(&self) -> Result<(String, usize)> {
        let mut out = ExportFile {
            meatshell_export: 1,
            sessions: self.cache.sessions.clone(),
        };
        for s in &mut out.sessions {
            // `cache` holds plaintext passwords; obfuscate with the export key.
            if !s.password.is_empty() {
                let enc = Self::encrypt_export(s.password.as_str())?;
                s.password = Secret::new(enc);
            }
            if !s.private_key_inline.is_empty() {
                let enc = Self::encrypt_export(s.private_key_inline.as_str())?;
                s.private_key_inline = Secret::new(enc);
            }
            // `last_used` is machine-local noise — don't carry it across.
            s.last_used = None;
        }
        Ok((serde_json::to_string_pretty(&out)?, out.sessions.len()))
    }

    /// Export all sessions to a portable JSON file. Passwords are re-encrypted
    /// with the built-in export key; everything else stays plaintext so the
    /// file is human-readable and editable. Returns the number of sessions.
    pub fn export_to(&self, path: &Path) -> Result<usize> {
        let (raw, count) = self.export_json()?;
        fs::write(path, raw).with_context(|| format!("failed to write {}", path.display()))?;
        Ok(count)
    }

    /// Import sessions from a MeatShell portable export or a FinalShell connection
    /// export. New sessions get fresh ids; duplicates (same host+user+port+kind)
    /// are skipped.
    /// Returns `(added, skipped)`. The store is saved if anything was added.
    pub fn import_json(&mut self, raw: &str) -> Result<(usize, usize)> {
        let (sessions, decrypt_meatshell_secrets) =
            match serde_json::from_str::<ExportFile>(raw) {
                Ok(file) => (file.sessions, true),
                Err(meatshell_error) => (
                    super::finalshell::parse_export(raw).with_context(|| {
                        format!(
                            "not a valid MeatShell or FinalShell export file; MeatShell parser: {meatshell_error}"
                        )
                    })?,
                    false,
                ),
            };

        let mut added = 0usize;
        let mut skipped = 0usize;
        for mut s in sessions {
            // Recover the plaintext password (cache stores plaintext). Accept an
            // export blob, our local enc:v1 blob, or a legacy plaintext value.
            // FinalShell's parser has already decrypted its DES password, so avoid
            // interpreting a coincidental `enc:*` plaintext prefix as ours.
            if decrypt_meatshell_secrets {
                if let Some(plain) = Self::decrypt_export(s.password.as_str()) {
                    s.password = Secret::new(plain);
                } else if let Some(plain) = Self::try_decrypt(&self.key, s.password.as_str()) {
                    s.password = Secret::new(plain);
                }
                if let Some(plain) = Self::decrypt_export(s.private_key_inline.as_str()) {
                    s.private_key_inline = Secret::new(plain);
                } else if let Some(plain) =
                    Self::try_decrypt(&self.key, s.private_key_inline.as_str())
                {
                    s.private_key_inline = Secret::new(plain);
                }
            }
            let dup = self.cache.sessions.iter().any(|x| {
                x.host == s.host && x.user == s.user && x.port == s.port && x.kind == s.kind
            });
            if dup {
                skipped += 1;
                continue;
            }
            s.id = Uuid::new_v4().to_string();
            self.upsert(s);
            added += 1;
        }
        if added > 0 {
            self.save()?;
        }
        Ok((added, skipped))
    }

    /// Import sessions from a MeatShell or FinalShell JSON export file.
    pub fn import_from(&mut self, path: &Path) -> Result<(usize, usize)> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        self.import_json(&raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> ConfigStore {
        let path = std::env::temp_dir().join(format!("ms-test-{}.json", Uuid::new_v4()));
        ConfigStore {
            path,
            backup_dir: None,
            cache: ConfigFile::default(),
            key: [7u8; 32],
        }
    }

    #[test]
    fn terminal_cursor_style_defaults_and_validates() {
        let mut store = temp_store();
        assert_eq!(store.terminal_cursor_style(), "block");

        store.set_terminal_cursor_style("bar".into());
        assert_eq!(store.terminal_cursor_style(), "bar");
        store.set_terminal_cursor_style("underline".into());
        assert_eq!(store.terminal_cursor_style(), "underline");
        store.set_terminal_cursor_style("unexpected".into());
        assert_eq!(store.terminal_cursor_style(), "block");

        store.cache = serde_json::from_str("{}").expect("legacy config must deserialize");
        assert_eq!(store.terminal_cursor_style(), "block");
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn renderer_mode_preserves_compatibility_default_and_validates() {
        let mut store = temp_store();
        assert_eq!(store.renderer_mode(), "software");

        store.set_renderer_mode("auto".into());
        assert_eq!(store.renderer_mode(), "auto");
        store.set_renderer_mode("gpu".into());
        assert_eq!(store.renderer_mode(), "gpu");
        store.set_renderer_mode("unexpected".into());
        assert_eq!(store.renderer_mode(), "software");

        store.cache = serde_json::from_str("{}").expect("legacy config must deserialize");
        assert_eq!(store.renderer_mode(), "software");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn renderer_mode_preserves_linux_automatic_default_and_validates() {
        let mut store = temp_store();
        assert_eq!(store.renderer_mode(), "auto");

        store.set_renderer_mode("gpu".into());
        assert_eq!(store.renderer_mode(), "gpu");
        store.set_renderer_mode("software".into());
        assert_eq!(store.renderer_mode(), "software");
        store.set_renderer_mode("unexpected".into());
        assert_eq!(store.renderer_mode(), "auto");

        store.cache = serde_json::from_str("{}").expect("legacy config must deserialize");
        assert_eq!(store.renderer_mode(), "auto");
    }

    #[test]
    fn quick_connect_groups_default_collapsed_and_remember_expansion() {
        let mut store = temp_store();
        store.cache.groups = vec!["production".into(), "staging".into()];
        store.cache.sessions.push(Session {
            group: "production".into(),
            ..sample_session("server")
        });

        assert!(store.collapsed_session_groups().is_none());
        store.set_session_group_collapsed("production", false);

        let collapsed = store.collapsed_session_groups().unwrap();
        assert!(!collapsed.iter().any(|group| group == "production"));
        assert!(collapsed.iter().any(|group| group == "staging"));
        assert!(collapsed.iter().any(|group| group == "system"));

        store.set_session_group_collapsed("production", true);
        assert!(store
            .collapsed_session_groups()
            .unwrap()
            .iter()
            .any(|group| group == "production"));
    }

    /// Display order for these tests: default(d1, d2), alpha(a1), beta(b1, b2)
    /// — "default" first, named groups alphabetically, stored Vec order
    /// matching display order.
    fn reorder_store() -> ConfigStore {
        let mut store = temp_store();
        store.cache.sessions = vec![
            sample_session("d1"),
            sample_session("d2"),
            Session {
                group: "alpha".into(),
                ..sample_session("a1")
            },
            Session {
                group: "beta".into(),
                ..sample_session("b1")
            },
            Session {
                group: "beta".into(),
                ..sample_session("b2")
            },
        ];
        // Empty collapse list = every group expanded.
        store.cache.collapsed_session_groups = Some(Vec::new());
        store
    }

    fn id_of(store: &ConfigStore, name: &str) -> String {
        store
            .sessions()
            .iter()
            .find(|s| s.name == name)
            .expect("session present")
            .id
            .clone()
    }

    fn order_of(store: &ConfigStore) -> Vec<(String, String)> {
        store
            .sessions()
            .iter()
            .map(|s| (s.name.clone(), s.group.clone()))
            .collect()
    }

    #[test]
    fn reorder_session_swaps_same_group_neighbours() {
        let mut store = reorder_store();
        assert!(store.reorder_session(&id_of(&store, "d1"), 1));
        assert_eq!(
            order_of(&store)[..2],
            [("d2".into(), "".into()), ("d1".into(), "".into())]
        );
    }

    #[test]
    fn reorder_session_hops_down_into_next_group_at_its_top() {
        let mut store = reorder_store();
        assert!(store.reorder_session(&id_of(&store, "d2"), 1));
        assert_eq!(
            order_of(&store)[1..4],
            [
                ("d2".into(), "alpha".into()),
                ("a1".into(), "alpha".into()),
                ("b1".into(), "beta".into()),
            ]
        );
    }

    #[test]
    fn reorder_session_hops_up_into_previous_group_at_its_bottom() {
        let mut store = reorder_store();
        assert!(store.reorder_session(&id_of(&store, "a1"), -1));
        assert_eq!(
            order_of(&store)[..3],
            [
                ("d1".into(), "".into()),
                ("d2".into(), "".into()),
                ("a1".into(), "".into()),
            ]
        );
    }

    #[test]
    fn reorder_session_skips_collapsed_groups() {
        let mut store = reorder_store();
        store.set_session_group_collapsed("alpha", true);
        assert!(store.reorder_session(&id_of(&store, "d2"), 1));
        assert_eq!(
            order_of(&store)[1..4],
            [
                ("a1".into(), "alpha".into()),
                ("d2".into(), "beta".into()),
                ("b1".into(), "beta".into()),
            ]
        );
    }

    #[test]
    fn reorder_session_lands_in_empty_explicit_folder() {
        let mut store = reorder_store();
        store.cache.groups.push("gamma".into());
        assert!(store.reorder_session(&id_of(&store, "b2"), 1));
        let last = store.sessions().iter().find(|s| s.name == "b2").unwrap();
        assert_eq!(last.group, "gamma");
    }

    #[test]
    fn reorder_session_keeps_emptied_implicit_group_as_folder() {
        let mut store = reorder_store();
        assert!(store.reorder_session(&id_of(&store, "a1"), 1));
        assert_eq!(
            store
                .sessions()
                .iter()
                .find(|s| s.name == "a1")
                .unwrap()
                .group,
            "beta"
        );
        assert!(store.groups().iter().any(|g| g == "alpha"));
    }

    #[test]
    fn reorder_session_no_op_at_list_edges() {
        let mut store = reorder_store();
        assert!(!store.reorder_session(&id_of(&store, "d1"), -1));
        assert!(!store.reorder_session(&id_of(&store, "b2"), 1));
        assert_eq!(order_of(&store).len(), 5);
    }

    #[test]
    fn reorder_session_cross_group_needs_a_collapse_list() {
        let mut store = reorder_store();
        store.cache.collapsed_session_groups = None;
        // Same-group hops still work ...
        assert!(store.reorder_session(&id_of(&store, "d1"), 1));
        // ... but crossing a boundary does not: no list means every group
        // renders collapsed (mirrors build_session_rows).
        assert!(!store.reorder_session(&id_of(&store, "b2"), 1));
    }

    #[test]
    fn issue_316_reserved_system_groups_are_repaired_and_rejected() {
        let mut system_session = sample_session("misfiled");
        system_session.group = "system".into();
        system_session.password = Secret::default();
        let mut default_session = sample_session("legacy-default");
        default_session.group = "Default".into();
        let mut cfg = ConfigFile {
            sessions: vec![system_session, default_session],
            groups: vec![
                "system".into(),
                "System".into(),
                "default".into(),
                "prod".into(),
            ],
            collapsed_session_groups: Some(vec!["system".into(), "prod".into()]),
            ..ConfigFile::default()
        };

        assert!(normalize_reserved_session_groups(&mut cfg));
        assert_eq!(cfg.groups, ["prod"]);
        assert!(cfg.sessions.iter().all(|session| session.group.is_empty()));
        assert!(cfg.sessions[0].password.is_empty());
        // The built-in system folder's collapse preference is display state,
        // not a user-created group, so normalization must preserve it.
        assert_eq!(
            cfg.collapsed_session_groups.as_deref(),
            Some(["system".to_string(), "prod".to_string()].as_slice())
        );

        let mut store = temp_store();
        store.add_group("system".into());
        store.add_group("DEFAULT".into());
        store.add_group("prod".into());
        store.rename_group("prod", "System".into());
        assert_eq!(store.groups(), ["prod"]);

        let mut session = sample_session("server");
        session.group = "SYSTEM".into();
        let id = session.id.clone();
        store.upsert(session);
        assert_eq!(store.get(&id).unwrap().group, "");
    }

    #[test]
    fn session_group_names_are_unique_case_insensitively() {
        let mut store = temp_store();
        store.add_group("Production".into());
        store.add_group("production".into());
        assert_eq!(store.groups(), ["Production"]);
        assert!(store.session_group_exists(" PRODUCTION "));

        let mut session = sample_session("staging-server");
        session.group = "Staging".into();
        store.upsert(session);
        assert!(store.session_group_exists("staging"));

        store.rename_group("Production", "STAGING".into());
        assert_eq!(store.groups(), ["Production"]);

        // Changing only the spelling/case of the same group remains valid.
        store.rename_group("Production", "production".into());
        assert_eq!(store.groups(), ["production"]);
    }

    #[test]
    fn macos_renderer_mode_defaults_to_cpu_and_preserves_gpu_choices() {
        assert_eq!(normalize_macos_renderer_mode(""), "software");
        assert_eq!(normalize_macos_renderer_mode("software"), "software");
        assert_eq!(normalize_macos_renderer_mode("femtovg"), "femtovg");
        assert_eq!(normalize_macos_renderer_mode("skia"), "skia");
        assert_eq!(normalize_macos_renderer_mode("unexpected"), "software");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn renderer_mode_uses_macos_backends_and_validates() {
        let mut store = temp_store();
        assert_eq!(store.renderer_mode(), "software");

        store.set_renderer_mode("skia".into());
        assert_eq!(store.renderer_mode(), "skia");
        store.set_renderer_mode("femtovg".into());
        assert_eq!(store.renderer_mode(), "femtovg");
        store.set_renderer_mode("software".into());
        assert_eq!(store.renderer_mode(), "software");
        store.set_renderer_mode("unexpected".into());
        assert_eq!(store.renderer_mode(), "software");

        store.cache = serde_json::from_str("{}").expect("legacy config must deserialize");
        assert_eq!(store.renderer_mode(), "software");
    }

    #[test]
    fn terminal_cursor_color_normalizes_and_rejects_invalid_values() {
        let mut store = temp_store();
        assert_eq!(store.terminal_cursor_color(), "");

        assert!(store.set_terminal_cursor_color("#1a2B3c"));
        assert_eq!(store.terminal_cursor_color(), "#1A2B3C");
        assert!(store.set_terminal_cursor_color("abcdef"));
        assert_eq!(store.terminal_cursor_color(), "#ABCDEF");

        assert!(!store.set_terminal_cursor_color("#12345"));
        assert_eq!(store.terminal_cursor_color(), "#ABCDEF");
        assert!(!store.set_terminal_cursor_color("#GG0000"));
        assert_eq!(store.terminal_cursor_color(), "#ABCDEF");
    }

    fn sample_session(name: &str) -> Session {
        Session {
            name: name.into(),
            host: "192.168.100.2".into(),
            port: 22,
            user: "root".into(),
            ..Session::new_empty()
        }
    }

    #[test]
    fn restores_and_syncs_user_config_backup() {
        let base = std::env::temp_dir().join(format!("ms-backup-{}", Uuid::new_v4()));
        let primary = base.join("portable");
        let backup = base.join("user");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(&backup).unwrap();

        let backup_cfg = ConfigFile {
            sessions: vec![sample_session("saved")],
            ..ConfigFile::default()
        };
        std::fs::write(
            backup.join("sessions.json"),
            serde_json::to_string_pretty(&backup_cfg).unwrap(),
        )
        .unwrap();
        std::fs::write(backup.join("secret.key"), [9u8; 32]).unwrap();

        restore_user_backup_if_needed(&primary, &backup);
        assert!(sessions_file_has_connections(
            &primary.join("sessions.json")
        ));
        assert_eq!(
            std::fs::read(primary.join("secret.key")).unwrap(),
            [9u8; 32]
        );

        let store = ConfigStore {
            path: primary.join("sessions.json"),
            backup_dir: Some(backup.clone()),
            cache: ConfigFile {
                sessions: vec![sample_session("new")],
                ..ConfigFile::default()
            },
            key: [7u8; 32],
        };
        std::fs::write(primary.join("secret.key"), [7u8; 32]).unwrap();
        store.save().unwrap();

        let raw = std::fs::read_to_string(backup.join("sessions.json")).unwrap();
        let cfg: ConfigFile = serde_json::from_str(&raw).unwrap();
        assert_eq!(cfg.sessions.len(), 1);
        assert_eq!(cfg.sessions[0].name, "new");
        assert_eq!(std::fs::read(backup.join("secret.key")).unwrap(), [7u8; 32]);

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn wallpaper_defaults_to_ms_but_keeps_explicit_choice() {
        // Fresh install (no file).
        let fresh = fresh_config();
        assert_eq!(fresh.wallpaper, "builtin:ms");
        assert!((fresh.wallpaper_overlay - 0.85).abs() < f32::EPSILON);
        // User upgrading from before the feature: JSON without the key.
        let cfg: ConfigFile = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg.wallpaper, "builtin:tech");
        // An explicit "无"/none (stored as "") is preserved, not re-defaulted.
        let cfg: ConfigFile = serde_json::from_str(r#"{"wallpaper":""}"#).unwrap();
        assert_eq!(cfg.wallpaper, "");
        // A custom choice is preserved.
        let cfg: ConfigFile = serde_json::from_str(r#"{"wallpaper":"builtin:light"}"#).unwrap();
        assert_eq!(cfg.wallpaper, "builtin:light");

        let mut cfg = ConfigFile {
            wallpaper: "builtin:miku".to_string(),
            defaults_rev: DEFAULTS_REV,
            ..ConfigFile::default()
        };
        assert!(!migrate_defaults(&mut cfg));
        assert_eq!(cfg.wallpaper, "builtin:miku");
    }

    #[test]
    fn wallpaper_transparency_default_migrates_without_overwriting_custom_value() {
        let mut old_default = ConfigFile {
            wallpaper_overlay: PREVIOUS_DEFAULT_WALLPAPER_OVERLAY,
            defaults_rev: 2,
            ..ConfigFile::default()
        };
        assert!(migrate_defaults(&mut old_default));
        assert!((old_default.wallpaper_overlay - 0.85).abs() < f32::EPSILON);

        let mut custom = ConfigFile {
            wallpaper_overlay: 0.70,
            defaults_rev: 2,
            ..ConfigFile::default()
        };
        assert!(migrate_defaults(&mut custom));
        assert!((custom.wallpaper_overlay - 0.70).abs() < f32::EPSILON);
    }

    #[test]
    fn output_highlight_defaults_and_preset_validation() {
        let mut store = temp_store();
        assert!(store.output_highlight_enabled());
        assert!(store.json_format_output());
        assert_eq!(store.output_highlight_preset(), "log");

        store.set_output_highlight_enabled(false);
        store.set_output_highlight_preset("devops".to_string());
        assert!(!store.output_highlight_enabled());
        store.set_json_format_output(false);
        assert!(!store.json_format_output());
        assert_eq!(store.output_highlight_preset(), "devops");

        store.set_output_highlight_preset("future-preset".to_string());
        assert_eq!(store.output_highlight_preset(), "log");

        store.add_output_highlight_rule(OutputHighlightRule {
            pattern: "  connection refused  ".to_string(),
            regex: false,
            case_sensitive: false,
            whole_line: true,
            color: "unknown".to_string(),
            enabled: true,
        });
        assert_eq!(store.output_highlight_rules().len(), 1);
        assert_eq!(
            store.output_highlight_rules()[0].pattern,
            "connection refused"
        );
        assert_eq!(store.output_highlight_rules()[0].color, "red");
        store.set_output_highlight_rule_enabled(0, false);
        assert!(!store.output_highlight_rules()[0].enabled);
        store.remove_output_highlight_rule(0);
        assert!(store.output_highlight_rules().is_empty());

        // An older settings file without either field retains the feature that
        // shipped in the previous version: enabled with the log preset.
        let legacy: ConfigFile = serde_json::from_str("{}").unwrap();
        store.cache = legacy;
        assert!(store.output_highlight_enabled());
        assert_eq!(store.output_highlight_preset(), "log");
    }

    #[test]
    fn saved_password_encrypts_and_decrypts_without_changes() {
        let mut store = temp_store();
        let password = "p@ss word!^&*中文";
        store.cache.sessions.push(Session {
            name: "windows-password".into(),
            host: "192.168.100.2".into(),
            port: 22,
            user: "root".into(),
            password: Secret::new(password),
            ..Session::new_empty()
        });

        store.save().unwrap();
        let raw = std::fs::read_to_string(&store.path).unwrap();
        assert!(!raw.contains(password));
        let disk: ConfigFile = serde_json::from_str(&raw).unwrap();
        let encrypted = disk.sessions[0].password.as_str();
        assert!(encrypted.starts_with(ConfigStore::ENC_PREFIX));
        assert_eq!(
            ConfigStore::try_decrypt(&store.key, encrypted).as_deref(),
            Some(password)
        );

        let _ = std::fs::remove_file(&store.path);
    }

    #[test]
    fn export_import_roundtrip_preserves_password() {
        let mut a = temp_store();
        a.cache.sessions.push(Session {
            name: "pve".into(),
            host: "192.168.100.2".into(),
            port: 22,
            user: "root".into(),
            password: Secret::new("s3cr3t"),
            ..Session::new_empty()
        });

        let export_path = std::env::temp_dir().join(format!("ms-exp-{}.json", Uuid::new_v4()));
        assert_eq!(a.export_to(&export_path).unwrap(), 1);

        // The file keeps host/user plaintext but the password is obfuscated.
        let raw = std::fs::read_to_string(&export_path).unwrap();
        assert!(raw.contains("192.168.100.2"));
        assert!(raw.contains(ConfigStore::EXPORT_PREFIX));
        assert!(!raw.contains("s3cr3t"));

        // Importing into a fresh store recovers the plaintext password.
        let mut b = temp_store();
        assert_eq!(b.import_from(&export_path).unwrap(), (1, 0));
        assert_eq!(b.cache.sessions.len(), 1);
        assert_eq!(b.cache.sessions[0].password.as_str(), "s3cr3t");
        assert_eq!(b.cache.sessions[0].host, "192.168.100.2");

        // Re-importing the same file skips the duplicate.
        assert_eq!(b.import_from(&export_path).unwrap(), (0, 1));

        let _ = std::fs::remove_file(&export_path);
        let _ = std::fs::remove_file(&a.path);
        let _ = std::fs::remove_file(&b.path);
    }

    #[test]
    fn imports_finalshell_export_and_reencrypts_password_at_rest() {
        let mut store = temp_store();
        let raw = r#"{
            "conection_type": 100,
            "name": "FinalShell host",
            "host": "192.0.2.20",
            "port": 22,
            "user_name": "operator",
            "password": "AwcLDRETFx1OXQgZJNatCplesw+x/P04",
            "authentication_type": 1,
            "terminal_encoding": "UTF-8"
        }"#;

        assert_eq!(store.import_json(raw).unwrap(), (1, 0));
        let session = &store.cache.sessions[0];
        assert_eq!(session.host, "192.0.2.20");
        assert_eq!(session.user, "operator");
        assert_eq!(session.password.as_str(), "meatshell-test");

        let saved = std::fs::read_to_string(&store.path).unwrap();
        assert!(!saved.contains("meatshell-test"));
        let disk: ConfigFile = serde_json::from_str(&saved).unwrap();
        assert!(disk.sessions[0]
            .password
            .as_str()
            .starts_with(ConfigStore::ENC_PREFIX));

        let _ = std::fs::remove_file(&store.path);
    }

    #[test]
    fn issue_300_interface_defaults_and_ranges_are_safe() {
        let mut store = temp_store();

        // Legacy configs keep the safe confirmation and familiar paste aliases.
        store.cache = serde_json::from_str("{}").unwrap();
        assert!(store.paste_confirm_enabled());
        assert!(store.extra_paste_shortcuts_enabled());
        assert!(!store.zen_mode());
        assert_eq!(store.terminal_line_spacing(), 1.0);

        store.set_terminal_line_spacing(0.1);
        assert_eq!(store.terminal_line_spacing(), 0.8);
        store.set_terminal_line_spacing(9.0);
        assert_eq!(store.terminal_line_spacing(), 1.5);

        store.set_paste_confirm_enabled(false);
        store.set_extra_paste_shortcuts_enabled(false);
        store.set_zen_mode(true);
        assert!(!store.paste_confirm_enabled());
        assert!(!store.extra_paste_shortcuts_enabled());
        assert!(store.zen_mode());
    }

    #[test]
    fn reorder_session_swaps_same_group_siblings_only() {
        let mut store = temp_store();
        let mk = |id: &str, group: &str| Session {
            id: id.into(),
            name: id.into(),
            group: group.into(),
            ..Session::new_empty()
        };
        store.cache.sessions = vec![mk("a", ""), mk("x", "ops"), mk("b", ""), mk("c", "")];

        // Moving "c" up swaps it with the nearest ungrouped sibling ("b"),
        // leaving the grouped session in between untouched.
        assert!(store.reorder_session("c", -1));
        assert_eq!(
            store
                .sessions()
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "x", "c", "b"]
        );
        // "a" is already the first ungrouped session; "x" is alone in its
        // group; unknown ids are no-ops.
        assert!(!store.reorder_session("a", -1));
        assert!(!store.reorder_session("x", 1));
        assert!(!store.reorder_session("nope", 1));
    }
}
