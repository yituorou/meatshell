use super::{blank_forward_draft, validated_port_forwards};

#[test]
fn blank_rows_are_ignored_when_saving() {
    assert!(validated_port_forwards(&[blank_forward_draft()])
        .unwrap()
        .is_empty());
}

#[test]
fn filled_rows_are_saved_without_an_add_step() {
    let mut local = blank_forward_draft();
    local.bind_port = "8080".into();
    local.host = "service.internal".into();
    local.host_port = "80".into();

    let mut dynamic = blank_forward_draft();
    dynamic.kind = "dynamic".into();
    dynamic.bind_port = "1080".into();

    let forwards = validated_port_forwards(&[local, dynamic]).unwrap();
    assert_eq!(forwards.len(), 2);
    assert_eq!(forwards[0].bind_port, 8080);
    assert_eq!(forwards[0].host, "service.internal");
    assert_eq!(forwards[1].kind, "dynamic");
    assert_eq!(forwards[1].host_port, 0);
}

#[test]
fn partially_filled_rows_block_saving() {
    let mut draft = blank_forward_draft();
    draft.bind_port = "8080".into();
    assert!(validated_port_forwards(&[draft]).is_err());
}

#[test]
fn legacy_saved_forward_starts_on_connect() {
    let forward: crate::config::PortForward = serde_json::from_str(
        r#"{"kind":"remote","bind_port":8080,"host":"127.0.0.1","host_port":80}"#,
    )
    .unwrap();
    assert!(forward.auto_start);
    assert!(forward.id.is_empty());
}

#[test]
fn edited_forward_keeps_identity_and_start_choice() {
    let mut draft = blank_forward_draft();
    draft.id = "saved-rule".into();
    draft.kind = "remote".into();
    draft.auto_start = false;
    draft.bind_port = "8080".into();
    draft.host = "127.0.0.1".into();
    draft.host_port = "80".into();
    let saved = validated_port_forwards(&[draft]).unwrap();
    assert_eq!(saved[0].id, "saved-rule");
    assert!(!saved[0].auto_start);
    let restored = super::forward_drafts(&saved);
    assert_eq!(restored[0].id, "saved-rule");
    assert!(!restored[0].auto_start);
}
