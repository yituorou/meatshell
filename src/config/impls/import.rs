//! Append-only session imports. Parse and validate the entire batch before either
//! the in-memory profile or its on-disk snapshot is changed.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{ConfigStore, Secret, Session, SessionKind};

/// Bound file reads as well as JSON parsing, including files that grow while read.
const MAX_IMPORT_BYTES: usize = 16 * 1024 * 1024;

/// Import results deliberately contain no connection details or credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ImportSummary {
    pub added: usize,
    pub skipped: usize,
}

#[derive(Deserialize)]
struct ImportedSessions {
    // Deliberately required: an arbitrary settings object is not a session export.
    sessions: Vec<Session>,
}

type SessionIdentity = String;

fn identity(session: &Session, sessions: &[Session]) -> Result<SessionIdentity> {
    // An endpoint is not a profile identity: users deliberately save aliases
    // with different names, credentials, groups, proxies and jump routes.
    // Compare the full configuration, excluding machine-local IDs/last_used.
    // Hash only in memory so secret values are not retained as lookup keys.
    fn fields(session: &Session) -> Result<serde_json::Value> {
        let mut value = serde_json::to_value(session)?;
        let object = value
            .as_object_mut()
            .expect("Session serializes as an object");
        for key in ["id", "last_used", "jump_session_id", "jump_session_ids"] {
            object.remove(key);
        }
        Ok(value)
    }
    let mut value = fields(session)?;
    let route = super::super::jump_chain::resolve_jump_chain(sessions, session)?;
    let route: Vec<_> = route.iter().map(fields).collect::<Result<_>>()?;
    value["resolved_jump_chain"] = serde_json::Value::Array(route);
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(&value)?)))
}

fn read_import(path: &Path) -> Result<String> {
    let metadata = fs::metadata(path).context("failed to inspect import file")?;
    if !metadata.is_file() {
        bail!("import source must be a regular JSON file");
    }
    if metadata.len() > MAX_IMPORT_BYTES as u64 {
        bail!("import file exceeds the 16 MiB limit");
    }
    let file = fs::File::open(path).context("failed to open import file")?;
    if !file
        .metadata()
        .context("failed to inspect import file")?
        .is_file()
    {
        bail!("import source must be a regular JSON file");
    }
    let mut bytes = Vec::new();
    file.take((MAX_IMPORT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .context("failed to read import file")?;
    if bytes.len() > MAX_IMPORT_BYTES {
        bail!("import file exceeds the 16 MiB limit");
    }
    // Do not attach UTF-8/serde errors: they can quote credential-bearing input.
    String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("import file must be UTF-8 JSON"))
}

impl ConfigStore {
    /// Import a portable MeatShell export, native sessions.json or FinalShell
    /// connection JSON. Global settings in native files are never imported.
    /// Dry-run performs the same parsing/validation without saving or mutating
    /// this store. Existing sessions are never overwritten.
    pub fn import_from_preview(&mut self, path: &Path, dry_run: bool) -> Result<ImportSummary> {
        self.import_json_preview(&read_import(path)?, dry_run)
    }

    pub fn import_json_preview(&mut self, raw: &str, dry_run: bool) -> Result<ImportSummary> {
        if raw.len() > MAX_IMPORT_BYTES {
            bail!("import file exceeds the 16 MiB limit");
        }
        let value: serde_json::Value = serde_json::from_str(raw).map_err(|error| {
            anyhow::anyhow!(
                "invalid import JSON at line {}, column {}",
                error.line(),
                error.column()
            )
        })?;
        let meatshell = value.get("meatshell_export").is_some() || value.get("sessions").is_some();
        let mut sessions = if meatshell {
            if let Some(version) = value.get("meatshell_export") {
                if version.as_u64() != Some(1) {
                    bail!("unsupported MeatShell export version; expected version 1");
                }
            }
            serde_json::from_value::<ImportedSessions>(value)
                .map_err(|_| anyhow::anyhow!("invalid MeatShell session fields in import file"))?
                .sessions
        } else {
            // FinalShell's decoder already returns plaintext. Never reinterpret
            // a coincidental enc:* prefix in that decoded password as our format.
            super::super::finalshell::parse_export(raw).map_err(|_| {
                anyhow::anyhow!("invalid or unsupported FinalShell import; check connection fields and password encoding")
            })?
        };

        let mut source_ids = HashSet::new();
        for (index, session) in sessions.iter_mut().enumerate() {
            if session.id.trim().is_empty() || !source_ids.insert(session.id.clone()) {
                bail!(
                    "import entry {} has an empty or repeated session ID",
                    index + 1
                );
            }
            match session.kind {
                SessionKind::Ssh | SessionKind::Telnet | SessionKind::Rdp => {
                    if session.host.trim().is_empty() || session.port == 0 {
                        bail!("import entry {} has an invalid host or port", index + 1);
                    }
                }
                SessionKind::Serial if session.serial_port.trim().is_empty() => {
                    bail!("import entry {} has an empty serial device", index + 1);
                }
                _ => {}
            }
            if meatshell {
                self.decode_import_secret(&mut session.password, index, "password")?;
                self.decode_import_secret(&mut session.private_key_inline, index, "private key")?;
                for trigger in &mut session.triggers {
                    self.decode_import_secret(&mut trigger.response, index, "trigger response")?;
                }
            }
        }

        let mut identities = HashMap::new();
        let mut used_ids = HashSet::new();
        for session in &self.cache.sessions {
            // An unrelated invalid existing route must not prevent importing
            // valid profiles, and is never used as a duplicate match.
            if let Ok(key) = identity(session, &self.cache.sessions) {
                identities.entry(key).or_insert_with(|| session.id.clone());
            }
            used_ids.insert(session.id.clone());
        }
        let existing_ids = used_ids.clone();
        let mut remapped_ids = HashMap::new();
        let mut keep = Vec::with_capacity(sessions.len());
        let mut summary = ImportSummary {
            added: 0,
            skipped: 0,
        };
        let mut source_graph = sessions.clone();
        source_graph.extend(
            self.cache
                .sessions
                .iter()
                .filter(|s| !source_ids.contains(&s.id))
                .cloned(),
        );
        // Pass one maps every source ID, including duplicates, before resolving
        // references. Forward references and references to skipped hops work.
        for session in &sessions {
            let key = identity(session, &source_graph)
                .map_err(|_| anyhow::anyhow!("import contains an invalid SSH jump chain"))?;
            let id = if let Some(existing) = identities.get(&key) {
                summary.skipped += 1;
                keep.push(false);
                existing.clone()
            } else {
                let id = loop {
                    let id = Uuid::new_v4().to_string();
                    if used_ids.insert(id.clone()) {
                        break id;
                    }
                };
                identities.insert(key, id.clone());
                summary.added += 1;
                keep.push(true);
                id
            };
            remapped_ids.insert(session.id.clone(), id);
        }

        let remap = |id: &str, index: usize| -> Result<String> {
            if let Some(mapped) = remapped_ids.get(id) {
                Ok(mapped.clone())
            } else if existing_ids.contains(id) {
                Ok(id.to_string())
            } else {
                bail!(
                    "import entry {} references a missing SSH jump session",
                    index + 1
                );
            }
        };
        for (index, session) in sessions.iter_mut().enumerate() {
            session.id = remapped_ids[&session.id].clone();
            if !session.jump_session_id.is_empty() {
                session.jump_session_id = remap(&session.jump_session_id, index)?;
            }
            for id in &mut session.jump_session_ids {
                *id = remap(id, index)?;
            }
        }

        let mut candidate = ConfigStore {
            path: self.path.clone(),
            backup_dir: self.backup_dir.clone(),
            cache: self.cache.clone(),
            disk_snapshot: std::cell::RefCell::new(self.disk_snapshot.borrow().clone()),
            key: self.key,
        };
        candidate.cache.sessions.extend(
            sessions
                .iter()
                .zip(&keep)
                .filter(|(_, keep)| **keep)
                .map(|(session, _)| session.clone()),
        );
        // Use the same graph checks as connection establishment, against the
        // final merged graph. Do not expose graph errors containing source IDs.
        for (index, session) in sessions.iter().enumerate() {
            candidate.resolve_jump_chain(session).map_err(|_| {
                anyhow::anyhow!("import entry {} has an invalid SSH jump chain (missing/non-SSH hop, cycle or too many hops)", index + 1)
            })?;
        }
        if !dry_run && summary.added > 0 {
            // save() checks the original disk snapshot under the profile lock.
            // Any validation, I/O or stale-write failure leaves self unchanged.
            candidate.save()?;
            self.cache = candidate.cache;
            *self.disk_snapshot.borrow_mut() = candidate.disk_snapshot.into_inner();
        }
        Ok(summary)
    }

    fn decode_import_secret(&self, secret: &mut Secret, index: usize, field: &str) -> Result<()> {
        let value = secret.as_str();
        let decoded = if value.starts_with(Self::EXPORT_PREFIX) {
            Some(Self::decrypt_export(value))
        } else if value.starts_with(Self::ENC_PREFIX) {
            Some(Self::try_decrypt(&self.key, value))
        } else if value.starts_with("enc:") {
            Some(None)
        } else {
            None
        };
        if let Some(decoded) = decoded {
            let plaintext = decoded.ok_or_else(|| {
                anyhow::anyhow!("cannot decrypt {} in import entry {}; use a portable MeatShell export or the matching local profile", field, index + 1)
            })?;
            *secret = Secret::new(plaintext);
        }
        Ok(())
    }

    /// Compatibility wrapper used by the GUI and WebDAV importer.
    pub fn import_json(&mut self, raw: &str) -> Result<(usize, usize)> {
        let summary = self.import_json_preview(raw, false)?;
        Ok((summary.added, summary.skipped))
    }

    /// Compatibility wrapper used by the GUI file picker.
    pub fn import_from(&mut self, path: &Path) -> Result<(usize, usize)> {
        let summary = self.import_from_preview(path, false)?;
        Ok((summary.added, summary.skipped))
    }
}

#[cfg(test)]
#[path = "import_tests.rs"]
mod tests;
