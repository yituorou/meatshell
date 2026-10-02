use super::{ConfigStore, Session, SessionKind};
use anyhow::{bail, Result};
use std::collections::HashSet;

/// Bound recursive connection/authentication work even for malformed imports.
const MAX_JUMP_HOPS: usize = 16;

impl ConfigStore {
    /// Immediate jump first; following entries are that jump's ancestors.
    /// Resolve the full graph before any network activity, never silently direct.
    pub fn resolve_jump_chain(&self, target: &Session) -> Result<Vec<Session>> {
        resolve_jump_chain(self.sessions(), target)
    }
}

fn resolve_jump_chain(sessions: &[Session], target: &Session) -> Result<Vec<Session>> {
    if target.kind != SessionKind::Ssh {
        return Ok(Vec::new());
    }
    if !target.jump_session_ids.is_empty() {
        return resolve_explicit_chain(sessions, target);
    }
    if target.jump_session_id.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut visited = HashSet::from([target.id.clone()]);
    let mut next = target.jump_session_id.as_str();
    let mut chain = Vec::new();
    while !next.trim().is_empty() {
        if !visited.insert(next.to_string()) {
            bail!("SSH jump chain contains a cycle at session {next}");
        }
        if chain.len() >= MAX_JUMP_HOPS {
            bail!("SSH jump chain exceeds {MAX_JUMP_HOPS} hops");
        }
        let hop = sessions
            .iter()
            .find(|s| s.id == next)
            .ok_or_else(|| anyhow::anyhow!("SSH jump session not found: {next}"))?;
        if hop.kind != SessionKind::Ssh {
            bail!("SSH jump session must use SSH: {}", hop.id);
        }
        chain.push(hop.clone());
        if !hop.jump_session_ids.is_empty() {
            for ancestor in resolve_explicit_chain(sessions, hop)? {
                if !visited.insert(ancestor.id.clone()) {
                    bail!("SSH jump chain contains a cycle at session {}", ancestor.id);
                }
                chain.push(ancestor);
            }
            if chain.len() > MAX_JUMP_HOPS {
                bail!("SSH jump chain exceeds {MAX_JUMP_HOPS} hops");
            }
            break;
        }
        next = hop.jump_session_id.as_str();
    }
    Ok(chain)
}

// Return immediate-hop-first, as required by SshSession::connect_via_chain.
fn resolve_explicit_chain(sessions: &[Session], target: &Session) -> Result<Vec<Session>> {
    if target.jump_session_ids.len() > MAX_JUMP_HOPS {
        bail!("SSH jump chain exceeds {MAX_JUMP_HOPS} hops");
    }
    let mut visited = HashSet::from([target.id.as_str()]);
    target
        .jump_session_ids
        .iter()
        .rev()
        .map(|id| {
            if id.trim().is_empty() {
                bail!("Select a session for every SSH jump hop");
            }
            if !visited.insert(id.as_str()) {
                bail!("SSH jump chain contains a cycle at session {id}");
            }
            let hop = sessions
                .iter()
                .find(|s| s.id == *id)
                .ok_or_else(|| anyhow::anyhow!("SSH jump session not found: {id}"))?;
            if hop.kind != SessionKind::Ssh {
                bail!("SSH jump session must use SSH: {id}");
            }
            Ok(hop.clone())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session(id: &str, jump: &str) -> Session {
        let mut s = Session::new_empty();
        s.id = id.into();
        s.jump_session_id = jump.into();
        s
    }
    #[test]
    fn direct_and_single_hop() {
        let direct = session("direct", "");
        assert!(resolve_jump_chain(&[], &direct).unwrap().is_empty());
        let target = session("target", "direct");
        assert_eq!(
            resolve_jump_chain(&[direct], &target).unwrap()[0].id,
            "direct"
        );
    }
    #[test]
    fn nested_hops_preserve_order_and_individual_credentials() {
        let target = session("target", "inner");
        let mut inner = session("inner", "outer");
        inner.user = "inside".into();
        let mut outer = session("outer", "");
        outer.user = "outside".into();
        let hops = resolve_jump_chain(&[outer, inner], &target).unwrap();
        assert_eq!(
            hops.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["inner", "outer"]
        );
        assert_eq!(
            hops.iter().map(|s| s.user.as_str()).collect::<Vec<_>>(),
            ["inside", "outside"]
        );
    }
    #[test]
    fn missing_inner_ancestor_fails_instead_of_direct_connect() {
        let target = session("target", "inner");
        assert!(resolve_jump_chain(&[], &target).is_err());
        let err = resolve_jump_chain(&[session("inner", "missing")], &target).unwrap_err();
        assert!(err.to_string().contains("missing"));
    }
    #[test]
    fn self_reference_and_long_cycles_are_rejected() {
        let target = session("target", "target");
        assert!(resolve_jump_chain(&[], &target)
            .unwrap_err()
            .to_string()
            .contains("cycle"));
        let target = session("target", "a");
        let saved = [session("a", "b"), session("b", "a")];
        assert!(resolve_jump_chain(&saved, &target)
            .unwrap_err()
            .to_string()
            .contains("cycle"));
    }
    #[test]
    fn non_ssh_hop_is_rejected() {
        let mut hop = session("serial", "");
        hop.kind = SessionKind::Serial;
        assert!(resolve_jump_chain(&[hop], &session("target", "serial")).is_err());
    }
    #[test]
    fn maximum_depth_is_bounded() {
        let mut saved: Vec<Session> = (0..MAX_JUMP_HOPS)
            .map(|i| session(&format!("h{i}"), if i == 0 { "" } else { "unused" }))
            .collect();
        for i in 1..saved.len() {
            saved[i].jump_session_id = format!("h{}", i - 1);
        }
        let target = session("target", &format!("h{}", MAX_JUMP_HOPS - 1));
        assert_eq!(
            resolve_jump_chain(&saved, &target).unwrap().len(),
            MAX_JUMP_HOPS
        );
        saved.push(target);
        let err = resolve_jump_chain(&saved, &session("extra", "target")).unwrap_err();
        assert!(err.to_string().contains("exceeds"));
    }
}

#[cfg(test)]
mod editor_tests {
    use super::*;

    fn saved(id: &str) -> Session {
        let mut value = Session::new_empty();
        value.id = id.into();
        value
    }

    #[test]
    fn explicit_order_overrides_inherited_routes_without_mutating_hops() {
        let mut outer = saved("a");
        outer.jump_session_id = "missing".into();
        let inner = saved("b");
        let mut target = saved("target");
        target.jump_session_ids = vec!["a".into(), "b".into()];
        let sessions = [outer, inner];
        let result = resolve_jump_chain(&sessions, &target).unwrap();
        assert_eq!(
            result.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["b", "a"]
        );
        target.jump_session_ids.reverse();
        let result = resolve_jump_chain(&sessions, &target).unwrap();
        assert_eq!(
            result.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(sessions[0].jump_session_id, "missing");
    }

    #[test]
    fn explicit_invalid_routes_fail_before_connecting() {
        let mut target = saved("target");
        let mut serial = saved("serial");
        serial.kind = SessionKind::Serial;
        let sessions = [saved("a"), serial];
        for ids in [
            vec![""],
            vec!["missing"],
            vec!["target"],
            vec!["serial"],
            vec!["a", "a"],
        ] {
            target.jump_session_ids = ids.into_iter().map(String::from).collect();
            assert!(resolve_jump_chain(&sessions, &target).is_err());
        }
        target.jump_session_ids = (0..17).map(|i| format!("h{i}")).collect();
        assert!(resolve_jump_chain(&sessions, &target)
            .unwrap_err()
            .to_string()
            .contains("exceeds"));
        target.jump_session_ids.clear();
        assert!(resolve_jump_chain(&sessions, &target).unwrap().is_empty());
    }

    #[test]
    fn legacy_target_can_reference_an_explicit_route_and_detect_cross_format_cycles() {
        let mut target = saved("target");
        target.jump_session_id = "b".into();
        let mut inner = saved("b");
        inner.jump_session_ids = vec!["a".into()];
        let hops = resolve_jump_chain(&[saved("a"), inner.clone()], &target).unwrap();
        assert_eq!(
            hops.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["b", "a"]
        );
        inner.jump_session_ids = vec!["target".into()];
        assert!(resolve_jump_chain(&[target.clone(), inner], &target).is_err());
    }

    #[test]
    fn saved_editor_options_round_trip_and_old_configs_default_to_no_reveal() {
        let mut target = saved("target");
        target.jump_session_ids = vec!["a".into(), "b".into()];
        target.allow_secret_reveal = true;
        let json = serde_json::to_value(&target).unwrap();
        let restored: Session = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(restored.jump_session_ids, ["a", "b"]);
        assert!(restored.allow_secret_reveal);
        let mut legacy = json;
        legacy.as_object_mut().unwrap().remove("jump_session_ids");
        legacy
            .as_object_mut()
            .unwrap()
            .remove("allow_secret_reveal");
        let restored: Session = serde_json::from_value(legacy).unwrap();
        assert!(restored.jump_session_ids.is_empty());
        assert!(!restored.allow_secret_reveal);
    }
}
