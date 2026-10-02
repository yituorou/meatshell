//! All credentials and endpoints below are synthetic fixtures.
use super::super::tests::temp_store;
use super::super::{ConfigFile, SessionTrigger};
use super::*;

fn session(id: &str) -> Session {
    Session {
        id: id.into(),
        name: format!("Synthetic {id}"),
        host: format!("{id}.example.invalid"),
        user: "fixture-user".into(),
        ..Session::new_empty()
    }
}

fn native(sessions: Vec<Session>) -> String {
    serde_json::json!({"sessions": sessions}).to_string()
}

fn snapshot(store: &ConfigStore) -> serde_json::Value {
    serde_json::to_value(&store.cache).unwrap()
}

fn remove_fixture(store: &ConfigStore) {
    let _ = fs::remove_file(&store.path);
    let _ = fs::remove_file(store.path.with_extension("json.lock"));
}

#[test]
fn portable_roundtrip_remaps_both_jump_formats_and_preserves_all_other_fields() {
    let mut source = temp_store();
    let outer = session("outer");
    let mut inner = session("inner");
    inner.jump_session_id = outer.id.clone();
    let mut target = session("target");
    target.jump_session_id = inner.id.clone();
    target.jump_session_ids = vec![outer.id.clone(), inner.id.clone()];
    target.password = Secret::new("synthetic-password");
    target.private_key_inline = Secret::new("synthetic-inline-key");
    target.private_key_path = "/synthetic/key/path".into();
    target.triggers.push(SessionTrigger {
        expect: "synthetic prompt".into(),
        response: Secret::new("synthetic-trigger-response"),
        append_enter: true,
        repeat: false,
    });
    target.group = "Synthetic Group".into();
    target.note = "Synthetic note".into();
    target.allow_secret_reveal = true;
    source.cache.sessions = vec![target.clone(), inner, outer];
    let (export, _) = source.export_json().unwrap();
    assert!(export.contains("enc:exp:v1:"));
    for secret in [
        "synthetic-password",
        "synthetic-inline-key",
        "synthetic-trigger-response",
    ] {
        assert!(!export.contains(secret));
    }

    let mut destination = temp_store();
    destination.key = [9; 32]; // Different local profile key, as on another machine.
    assert_eq!(destination.import_json(&export).unwrap(), (3, 0));
    let imported = &destination.cache.sessions;
    assert_ne!(imported[0].id, "target");
    assert_eq!(imported[0].jump_session_id, imported[1].id);
    assert_eq!(
        imported[0].jump_session_ids,
        [imported[2].id.clone(), imported[1].id.clone()]
    );
    assert_eq!(imported[1].jump_session_id, imported[2].id);
    assert_eq!(
        destination.resolve_jump_chain(&imported[0]).unwrap().len(),
        2
    );
    let mut comparable = imported[0].clone();
    comparable.id = target.id.clone();
    comparable.jump_session_id = target.jump_session_id.clone();
    comparable.jump_session_ids = target.jump_session_ids.clone();
    assert_eq!(
        serde_json::to_value(comparable).unwrap(),
        serde_json::to_value(target).unwrap()
    );
    let disk: ConfigFile =
        serde_json::from_str(&fs::read_to_string(&destination.path).unwrap()).unwrap();
    assert_eq!(
        ConfigStore::try_decrypt(&destination.key, disk.sessions[0].password.as_str()).as_deref(),
        Some("synthetic-password")
    );
    assert_eq!(
        ConfigStore::try_decrypt(
            &destination.key,
            disk.sessions[0].private_key_inline.as_str()
        )
        .as_deref(),
        Some("synthetic-inline-key")
    );
    assert_eq!(
        ConfigStore::try_decrypt(
            &destination.key,
            disk.sessions[0].triggers[0].response.as_str()
        )
        .as_deref(),
        Some("synthetic-trigger-response")
    );
    remove_fixture(&destination);
}

#[test]
fn duplicates_keep_existing_ids_credentials_and_remap_new_hops() {
    let mut store = temp_store();
    let mut existing = session("existing");
    existing.password = Secret::new("existing-synthetic-password");
    existing.group = "Existing Group".into();
    store.cache.sessions.push(existing.clone());
    store.save().unwrap();
    let mut duplicated_hop = existing.clone();
    duplicated_hop.id = "imported-hop".into();
    let mut target = session("target");
    target.jump_session_id = duplicated_hop.id.clone();
    target.jump_session_ids = vec![duplicated_hop.id.clone()];
    let raw = native(vec![target, duplicated_hop]);
    assert_eq!(store.import_json(&raw).unwrap(), (1, 1));
    assert_eq!(
        serde_json::to_value(&store.cache.sessions[0]).unwrap(),
        serde_json::to_value(existing).unwrap()
    );
    assert_eq!(store.cache.sessions[1].jump_session_id, "existing");
    assert_eq!(store.cache.sessions[1].jump_session_ids, ["existing"]);
    let before = fs::read(&store.path).unwrap();
    assert_eq!(store.import_json(&raw).unwrap(), (0, 2));
    assert_eq!(fs::read(&store.path).unwrap(), before);
    remove_fixture(&store);
}

#[test]
fn intra_batch_duplicates_are_skipped_and_references_use_the_kept_session() {
    let mut store = temp_store();
    let first = session("first");
    let mut duplicate = first.clone();
    duplicate.id = "duplicate".into();
    let mut target = session("target");
    target.jump_session_ids = vec![duplicate.id.clone()];
    assert_eq!(
        store
            .import_json(&native(vec![target, first, duplicate]))
            .unwrap(),
        (2, 1)
    );
    assert_eq!(
        store.cache.sessions[0].jump_session_ids,
        [store.cache.sessions[1].id.clone()]
    );
    remove_fixture(&store);
}

#[test]
fn native_import_preserves_destination_settings_and_existing_records() {
    let mut store = temp_store();
    store.cache.wallpaper = "synthetic-destination-wallpaper".into();
    store.cache.groups = vec!["Existing Group".into()];
    store.cache.mcp_enabled = false;
    store.cache.mcp_allow_commands = false;
    store.cache.webdav_password = Secret::new("existing-synthetic-webdav-secret");
    store.cache.sessions.push(session("existing"));
    let before = snapshot(&store);
    // Native global settings are ignored even if their types/credentials are not
    // valid for this destination. Only the sessions field is read.
    let raw = serde_json::json!({
        "sessions": [session("new")],
        "wallpaper": 42,
        "mcp_enabled": true,
        "mcp_allow_commands": true,
        "groups": ["Replacement Group"],
        "webdav_password": "enc:v1:foreign-global-secret"
    })
    .to_string();
    assert_eq!(store.import_json(&raw).unwrap(), (1, 0));
    let mut after = snapshot(&store);
    after["sessions"] = before["sessions"].clone();
    assert_eq!(after, before);
    remove_fixture(&store);
}

#[test]
fn dry_run_is_repeatable_and_never_changes_cache_disk_or_snapshot() {
    let mut store = temp_store();
    store.cache.sessions.push(session("existing"));
    store.save().unwrap();
    let before = snapshot(&store);
    let disk = fs::read(&store.path).unwrap();
    let disk_snapshot = store.disk_snapshot.borrow().clone();
    let raw = native(vec![session("existing"), session("new")]);
    for _ in 0..2 {
        assert_eq!(
            store.import_json_preview(&raw, true).unwrap(),
            ImportSummary {
                added: 1,
                skipped: 1
            }
        );
        assert_eq!(snapshot(&store), before);
        assert_eq!(fs::read(&store.path).unwrap(), disk);
        assert_eq!(*store.disk_snapshot.borrow(), disk_snapshot);
    }
    remove_fixture(&store);
    let mut fresh = temp_store();
    assert_eq!(
        fresh
            .import_json_preview(&native(vec![session("fresh")]), true)
            .unwrap()
            .added,
        1
    );
    assert!(!fresh.path.exists());
    assert!(!fresh.path.with_extension("json.lock").exists());
}

#[test]
fn invalid_secret_aborts_entire_batch_including_duplicates_and_dry_run() {
    for field in ["password", "private_key_inline", "trigger"] {
        for secret in [
            "enc:v1:foreign-or-corrupt",
            "enc:exp:v1:corrupt",
            "enc:future:unsupported",
        ] {
            for dry_run in [false, true] {
                let mut store = temp_store();
                store.cache.sessions.push(session("existing"));
                store.save().unwrap();
                let before = snapshot(&store);
                let disk = fs::read(&store.path).unwrap();
                let mut invalid = session("existing"); // Still validate skipped entries.
                match field {
                    "password" => invalid.password = Secret::new(secret),
                    "private_key_inline" => invalid.private_key_inline = Secret::new(secret),
                    _ => invalid.triggers.push(SessionTrigger {
                        response: Secret::new(secret),
                        ..SessionTrigger::default()
                    }),
                }
                let error = store
                    .import_json_preview(&native(vec![session("valid"), invalid]), dry_run)
                    .unwrap_err();
                assert!(!format!("{error:#}").contains(secret));
                assert_eq!(snapshot(&store), before);
                assert_eq!(fs::read(&store.path).unwrap(), disk);
                remove_fixture(&store);
            }
        }
    }
}

#[test]
fn native_same_profile_ciphertext_decrypts_but_foreign_key_aborts() {
    let mut store = temp_store();
    let mut imported = session("native");
    imported.password =
        Secret::new(ConfigStore::encrypt(&store.key, "synthetic-local-password").unwrap());
    assert_eq!(store.import_json(&native(vec![imported])).unwrap(), (1, 0));
    assert_eq!(
        store.cache.sessions[0].password.as_str(),
        "synthetic-local-password"
    );
    let before = snapshot(&store);
    let disk = fs::read(&store.path).unwrap();
    let mut foreign = session("foreign");
    foreign.password =
        Secret::new(ConfigStore::encrypt(&[99; 32], "synthetic-foreign-password").unwrap());
    assert!(store.import_json(&native(vec![foreign])).is_err());
    assert_eq!(snapshot(&store), before);
    assert_eq!(fs::read(&store.path).unwrap(), disk);
    remove_fixture(&store);
}

#[test]
fn save_failure_and_stale_profile_leave_import_cache_unchanged() {
    let mut store = temp_store();
    store.cache.sessions.push(session("existing"));
    store.save().unwrap();
    let before = snapshot(&store);
    let disk = fs::read(&store.path).unwrap();
    // A directory at the atomic-write target causes a deterministic I/O failure,
    // even when tests run as an account that can bypass file permissions.
    fs::create_dir(store.path.with_extension("json.tmp")).unwrap();
    assert!(store.import_json(&native(vec![session("new")])).is_err());
    assert_eq!(snapshot(&store), before);
    assert_eq!(fs::read(&store.path).unwrap(), disk);
    fs::remove_dir(store.path.with_extension("json.tmp")).unwrap();
    let external = native(vec![session("external")]);
    fs::write(&store.path, &external).unwrap();
    assert!(store.import_json(&native(vec![session("new")])).is_err());
    assert_eq!(snapshot(&store), before);
    assert_eq!(fs::read_to_string(&store.path).unwrap(), external);
    remove_fixture(&store);
}

#[test]
fn invalid_graphs_and_ambiguous_ids_are_rejected_without_mutation() {
    let mut cases = Vec::new();
    let mut missing = session("missing");
    missing.jump_session_id = "absent".into();
    cases.push(vec![missing]);
    let mut self_cycle = session("self");
    self_cycle.jump_session_ids = vec!["self".into()];
    cases.push(vec![self_cycle]);
    let mut a = session("a");
    let mut b = session("b");
    a.jump_session_id = b.id.clone();
    b.jump_session_id = a.id.clone();
    cases.push(vec![a, b]);
    cases.push(vec![session("repeated"), session("repeated")]);
    let mut empty_id = session("empty");
    empty_id.id.clear();
    cases.push(vec![empty_id]);
    let mut non_ssh = session("nonssh");
    non_ssh.kind = SessionKind::Telnet;
    let mut target = session("target");
    target.jump_session_ids = vec![non_ssh.id.clone()];
    cases.push(vec![target, non_ssh]);
    let mut too_long: Vec<Session> = (0..18).map(|i| session(&format!("hop-{i}"))).collect();
    too_long[0].jump_session_ids = too_long[1..].iter().map(|s| s.id.clone()).collect();
    cases.push(too_long);
    for batch in cases {
        let mut store = temp_store();
        let before = snapshot(&store);
        assert!(store.import_json(&native(batch)).is_err());
        assert_eq!(snapshot(&store), before);
        assert!(!store.path.exists());
    }
}

#[test]
fn parse_errors_never_include_untrusted_secret_values_even_in_error_chain() {
    let sentinel = "SYNTHETIC_SECRET_MUST_NOT_APPEAR";
    let mut bad_session = serde_json::to_value(session("bad")).unwrap();
    bad_session["kind"] = serde_json::json!(sentinel);
    let inputs = [
        serde_json::json!({"meatshell_export": 1, "sessions": [bad_session]}).to_string(),
        serde_json::json!({"conection_type": 100, "host": "fixture.example.invalid", "password": {sentinel: sentinel}}).to_string(),
        format!("{{\"{sentinel}\": invalid-json}}"),
        serde_json::json!({"meatshell_export": sentinel, "sessions": []}).to_string(),
    ];
    for input in inputs {
        let mut store = temp_store();
        let error = store.import_json(&input).unwrap_err();
        assert!(!format!("{error:#}").contains(sentinel));
        assert!(!format!("{error:?}").contains(sentinel));
        assert!(!store.path.exists());
    }
}

#[test]
fn rejects_unknown_export_versions_and_missing_session_array() {
    let mut store = temp_store();
    for input in [
        r#"{"meatshell_export":2,"sessions":[]}"#,
        r#"{"meatshell_export":1}"#,
        "{}",
        r#"{"sessions":null}"#,
    ] {
        assert!(store.import_json(input).is_err());
    }
    assert_eq!(store.import_json(r#"{"sessions":[]}"#).unwrap(), (0, 0));
    assert!(!store.path.exists());
}

#[test]
fn file_import_is_bounded_and_requires_regular_utf8_json() {
    let mut store = temp_store();
    let path = std::env::temp_dir().join(format!("ms-import-{}.json", Uuid::new_v4()));
    fs::write(&path, native(vec![session("from-file")])).unwrap();
    assert_eq!(store.import_from_preview(&path, true).unwrap().added, 1);
    assert!(!store.path.exists());
    let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len((MAX_IMPORT_BYTES + 1) as u64).unwrap();
    drop(file);
    assert!(store
        .import_from(&path)
        .unwrap_err()
        .to_string()
        .contains("16 MiB"));
    assert!(store
        .import_json(&" ".repeat(MAX_IMPORT_BYTES + 1))
        .is_err());
    fs::write(&path, [0xff, 0xfe]).unwrap();
    assert!(store.import_from(&path).is_err());
    fs::remove_file(&path).unwrap();
    assert!(store.import_from(&path).is_err());
    fs::create_dir(&path).unwrap();
    assert!(store.import_from(&path).is_err());
    fs::remove_dir(&path).unwrap();
    assert!(!store.path.exists());
}

#[test]
fn invalid_finalshell_batch_is_atomic_and_redacted() {
    let mut store = temp_store();
    let raw = r#"[
        {"conection_type":100,"host":"valid.example.invalid","password":"AwcLDRETFx1OXQgZJNatCplesw+x/P04"},
        {"conection_type":100,"host":"SYNTHETIC_SECRET_IN_HOST","password":"not base64"}
    ]"#;
    let error = store.import_json(raw).unwrap_err();
    assert!(!format!("{error:#}").contains("SYNTHETIC_SECRET"));
    assert!(store.sessions().is_empty());
    assert!(!store.path.exists());
}

#[test]
fn decoded_literal_ciphertext_prefixes_are_encrypted_on_import() {
    let literal = "enc:v1:synthetic-literal-password-not-ciphertext";
    let mut source = temp_store();
    let mut imported = session("literal-prefix");
    imported.password = Secret::new(literal);
    imported.private_key_inline = Secret::new(literal);
    imported.triggers.push(SessionTrigger {
        response: Secret::new(literal),
        ..SessionTrigger::default()
    });
    source.cache.sessions.push(imported);
    let (export, _) = source.export_json().unwrap();
    let mut destination = temp_store();
    destination.key = [9; 32];
    assert_eq!(destination.import_json(&export).unwrap(), (1, 0));
    assert_eq!(destination.sessions()[0].password.as_str(), literal);
    let raw = fs::read_to_string(&destination.path).unwrap();
    assert!(!raw.contains(literal));
    let disk: ConfigFile = serde_json::from_str(&raw).unwrap();
    for secret in [
        &disk.sessions[0].password,
        &disk.sessions[0].private_key_inline,
        &disk.sessions[0].triggers[0].response,
    ] {
        assert_eq!(
            ConfigStore::try_decrypt(&destination.key, secret.as_str()).as_deref(),
            Some(literal)
        );
    }
    remove_fixture(&destination);
}

#[test]
fn literal_encryption_prefix_stays_protected_across_load_and_ordinary_saves() {
    let literal = "enc:v1:synthetic-literal-credential";
    let mut store = temp_store();
    store.cache.webdav_password = Secret::new(literal);
    let mut imported = session("repeated-save");
    imported.password = Secret::new(literal);
    imported.private_key_inline = Secret::new(literal);
    imported.triggers.push(SessionTrigger {
        response: Secret::new(literal),
        ..SessionTrigger::default()
    });
    let mut source = temp_store();
    source.cache.sessions.push(imported);
    let (export, _) = source.export_json().unwrap();
    store.import_json(&export).unwrap();
    for _ in 0..2 {
        let raw = fs::read_to_string(&store.path).unwrap();
        assert!(!raw.contains(literal));
        // Exercise the exact deserialization/decryption helper used by load(),
        // without ever resolving or writing the process-global user profile.
        let mut loaded: ConfigFile = serde_json::from_str(&raw).unwrap();
        assert!(loaded.sessions[0].password.is_local_ciphertext());
        ConfigStore::decrypt_local_secrets(&store.key, &mut loaded);
        for secret in [
            &loaded.sessions[0].password,
            &loaded.sessions[0].private_key_inline,
            &loaded.sessions[0].triggers[0].response,
            &loaded.webdav_password,
        ] {
            assert_eq!(secret.as_str(), literal);
            assert!(!secret.is_local_ciphertext());
        }
        store.cache = loaded;
        store.save().unwrap();
    }
    assert!(!fs::read_to_string(&store.path).unwrap().contains(literal));
    remove_fixture(&store);
}

#[test]
fn saving_import_preserves_undecodable_existing_local_ciphertext() {
    let opaque = "enc:v1:synthetic-unknown-key-ciphertext";
    let mut store = temp_store();
    store.cache = serde_json::from_str(&native(vec![session("existing")])).unwrap();
    store.cache.sessions[0].password = serde_json::from_value(serde_json::json!(opaque)).unwrap();
    ConfigStore::decrypt_local_secrets(&store.key, &mut store.cache);
    assert!(store.cache.sessions[0].password.is_local_ciphertext());
    assert_eq!(
        store.import_json(&native(vec![session("new")])).unwrap(),
        (1, 0)
    );
    let disk: ConfigFile = serde_json::from_str(&fs::read_to_string(&store.path).unwrap()).unwrap();
    assert_eq!(disk.sessions[0].password.as_str(), opaque);
    assert_eq!(store.cache.sessions[0].password.as_str(), opaque);
    remove_fixture(&store);
}

#[test]
fn same_endpoint_profiles_preserve_names_auth_credentials_proxy_and_routes() {
    let mut store = temp_store();
    let original = session("original");
    let mut aliases = Vec::new();
    for variant in 0..6 {
        let mut alias = original.clone();
        alias.id = format!("alias-{variant}");
        match variant {
            0 => alias.name = "Intentional alias".into(),
            1 => alias.group = "Different group".into(),
            2 => alias.auth = super::super::AuthMethod::Key,
            3 => alias.password = Secret::new("different-synthetic-password"),
            4 => alias.proxy = "socks5://127.0.0.1:1080".into(),
            _ => alias.jump_session_ids = vec!["hop".into()],
        }
        aliases.push(alias);
    }
    let mut batch = vec![original.clone(), session("hop")];
    batch.extend(aliases);
    let raw = native(batch);
    // Adopt one unchanged profile already imported by the legacy version.
    let mut existing = original;
    existing.id = "existing-local-id".into();
    store.cache.sessions.push(existing);
    store.save().unwrap();
    assert_eq!(store.import_json(&raw).unwrap(), (7, 1));
    assert_eq!(store.cache.sessions.len(), 8);
    assert_eq!(store.cache.sessions[0].id, "existing-local-id");
    let ids: Vec<_> = store.cache.sessions.iter().map(|s| s.id.clone()).collect();
    assert_eq!(store.import_json(&raw).unwrap(), (0, 8));
    assert_eq!(
        store
            .cache
            .sessions
            .iter()
            .map(|s| s.id.clone())
            .collect::<Vec<_>>(),
        ids
    );
    remove_fixture(&store);
}
