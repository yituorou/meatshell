use super::*;

/// Keep only a process-local hash, never another copy of draft credentials.
/// Slint changed handlers are deferred: matching snapshots prevent an older
/// edit notification from cancelling a test just started for those same values.
pub(super) fn connection_snapshot(window: &AppWindow) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    for value in [
        window.get_dialog_kind(),
        window.get_dialog_host(),
        window.get_dialog_port(),
        window.get_dialog_user(),
        window.get_dialog_auth(),
        window.get_dialog_password(),
        window.get_dialog_key_path(),
        window.get_dialog_key_inline(),
        window.get_dialog_proxy_type(),
        window.get_dialog_proxy_hostport(),
        window.get_dialog_serial_port(),
        window.get_dialog_baud(),
        window.get_dialog_data_bits(),
        window.get_dialog_stop_bits(),
        window.get_dialog_parity(),
        window.get_dialog_flow(),
    ] {
        value.as_str().hash(&mut hash);
    }
    window.get_dialog_key_inline_mode().hash(&mut hash);
    hash.finish()
}

pub(super) fn stop_test(
    window: &AppWindow,
    window_id: u64,
    test: &RefCell<crate::session_test::EditorTest>,
) -> bool {
    let old_test = test.borrow_mut().stop();
    if let Some(id) = old_test {
        abort_test_prompts(window, window_id, id);
        true
    } else {
        false
    }
}

pub(super) fn finish_test(
    weak: slint::Weak<AppWindow>,
    window_id: u64,
    ticket: crate::session_test::TestTicket,
    message: String,
) {
    let _ = slint::invoke_from_event_loop(move || {
        if !ticket.finish() {
            return;
        }
        if let Some(window) = weak.upgrade() {
            abort_test_prompts(&window, window_id, ticket.id);
            if window.get_dialog_open() && ticket.matches_snapshot(connection_snapshot(&window)) {
                window.set_dialog_test_status(message.into());
            }
        }
    });
}

/// Preserve dangling ids as an unselected row so saving cannot silently go direct.
pub(super) fn jump_rows(ids: Vec<String>, choices: &ModelRc<SharedString>) -> ModelRc<JumpHop> {
    ModelRc::from(Rc::new(VecModel::from(
        ids.into_iter()
            .map(|id| {
                let choice = choices
                    .iter()
                    .position(|value| value.as_str() == id)
                    .map(|index| index as i32)
                    .unwrap_or(0);
                JumpHop {
                    id: id.into(),
                    choice,
                    placeholder: false,
                }
            })
            .collect::<Vec<_>>(),
    )))
}

fn update_rows(window: &AppWindow, update: impl FnOnce(&mut Vec<JumpHop>)) {
    let mut rows = window.get_dialog_jumps().iter().collect::<Vec<_>>();
    update(&mut rows);
    window.set_dialog_jumps(ModelRc::from(Rc::new(VecModel::from(rows))));
    window.set_dialog_test_status("".into());
}

pub(super) fn move_row<T>(rows: &mut Vec<T>, from: i32, to: i32) {
    if from >= 0 && to >= 0 && (from as usize) < rows.len() && (to as usize) < rows.len() {
        let row = rows.remove(from as usize);
        rows.insert(to as usize, row);
    }
}

pub(super) fn register(
    window: &AppWindow,
    store: Rc<RefCell<ConfigStore>>,
    window_id: u64,
    test: Rc<RefCell<crate::session_test::EditorTest>>,
) {
    let weak = window.as_weak();
    let active_test = test.clone();
    window.on_add_jump(move || {
        if let Some(w) = weak.upgrade() {
            stop_test(&w, window_id, &active_test);
            update_rows(&w, |rows| {
                if rows.len() < 16 {
                    rows.push(JumpHop {
                        placeholder: true,
                        ..JumpHop::default()
                    });
                }
            });
        }
    });
    let weak = window.as_weak();
    let active_test = test.clone();
    window.on_update_jump(move |index, choice| {
        if let Some(w) = weak.upgrade() {
            stop_test(&w, window_id, &active_test);
            let Some(id) = w.get_jump_ids().row_data(choice.max(0) as usize) else {
                return;
            };
            update_rows(&w, |rows| {
                if index >= 0 {
                    if let Some(row) = rows.get_mut(index as usize) {
                        let placeholder = row.placeholder && id.trim().is_empty();
                        *row = JumpHop {
                            id,
                            choice,
                            placeholder,
                        };
                    }
                }
            });
        }
    });
    let weak = window.as_weak();
    let active_test = test.clone();
    window.on_delete_jump(move |index| {
        if let Some(w) = weak.upgrade() {
            stop_test(&w, window_id, &active_test);
            update_rows(&w, |rows| {
                if index >= 0 && (index as usize) < rows.len() {
                    rows.remove(index as usize);
                }
            });
        }
    });
    let weak = window.as_weak();
    let active_test = test.clone();
    window.on_move_jump(move |from, to| {
        if let Some(w) = weak.upgrade() {
            stop_test(&w, window_id, &active_test);
            update_rows(&w, |rows| move_row(rows, from, to));
        }
    });
    let weak = window.as_weak();
    window.on_reveal_session_secret(move |private_key| {
        let Some(w) = weak.upgrade() else {
            return SharedString::new();
        };
        // A reveal request must come from an open editor after explicit opt-in.
        if !w.get_dialog_open() || !w.get_dialog_allow_secret_reveal() {
            return SharedString::new();
        }
        let store = store.borrow();
        let Some(session) = store.get(w.get_dialog_id().as_str()) else {
            return SharedString::new();
        };
        if private_key {
            session.private_key_inline.as_str().into()
        } else {
            session.password.as_str().into()
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moving_hops_uses_final_drop_index_and_rejects_invalid_indices() {
        let mut rows = vec!["a", "b", "c"];
        move_row(&mut rows, 0, 2);
        assert_eq!(rows, ["b", "c", "a"]);
        move_row(&mut rows, 2, 0);
        assert_eq!(rows, ["a", "b", "c"]);
        move_row(&mut rows, -1, 1);
        move_row(&mut rows, 0, 3);
        assert_eq!(rows, ["a", "b", "c"]);
    }

    #[test]
    fn saving_unrevealed_or_revealed_secrets_preserves_credentials_and_route() {
        let mut saved = Session::new_empty();
        saved.password = Secret::new("fixture-password");
        saved.private_key_inline = Secret::new("fixture-key");
        let mut draft = SessionDraft::default();
        draft.id = saved.id.clone().into();
        draft.kind = "ssh".into();
        draft.auth = "key".into();
        draft.private_key_inline_mode = true;
        draft.allow_secret_reveal = true;
        draft.session_log = "on".into();
        draft.jumps = jump_rows(vec!["a".into(), "b".into()], &ModelRc::default());
        let session = session_from_draft(&draft, Some(&saved), vec![], vec![]);
        assert_eq!(session.password.as_str(), "fixture-password");
        assert_eq!(session.private_key_inline.as_str(), "fixture-key");
        assert_eq!(session.jump_session_ids, ["a", "b"]);
        assert!(session.jump_session_id.is_empty());
        assert!(session.allow_secret_reveal);
        assert_eq!(session.session_log, SessionLogMode::On);
        draft.password = "replacement".into();
        draft.private_key_inline = "replacement-key".into();
        let session = session_from_draft(&draft, Some(&saved), vec![], vec![]);
        assert_eq!(session.password.as_str(), "replacement");
        assert_eq!(session.private_key_inline.as_str(), "replacement-key");
        draft.jumps = ModelRc::default();
        let session = session_from_draft(&draft, Some(&saved), vec![], vec![]);
        assert!(session.jump_session_ids.is_empty());
    }

    #[test]
    fn dangling_hop_stays_in_draft_until_explicitly_removed() {
        let choices = ModelRc::from(Rc::new(VecModel::from(vec!["".into(), "a".into()])));
        let rows = jump_rows(vec!["missing".into(), "a".into()], &choices);
        assert_eq!(rows.row_data(0).unwrap().id, "missing");
        assert_eq!(rows.row_data(0).unwrap().choice, 0);
        assert_eq!(rows.row_data(1).unwrap().choice, 1);
    }
}
