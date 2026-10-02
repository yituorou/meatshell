use super::*;

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

pub(super) fn register(window: &AppWindow, store: Rc<RefCell<ConfigStore>>) {
    let weak = window.as_weak();
    window.on_add_jump(move || {
        if let Some(w) = weak.upgrade() {
            update_rows(&w, |rows| {
                if rows.len() < 16 {
                    rows.push(JumpHop::default());
                }
            });
        }
    });
    let weak = window.as_weak();
    window.on_update_jump(move |index, choice| {
        if let Some(w) = weak.upgrade() {
            let Some(id) = w.get_jump_ids().row_data(choice.max(0) as usize) else {
                return;
            };
            update_rows(&w, |rows| {
                if index >= 0 {
                    if let Some(row) = rows.get_mut(index as usize) {
                        *row = JumpHop { id, choice };
                    }
                }
            });
        }
    });
    let weak = window.as_weak();
    window.on_delete_jump(move |index| {
        if let Some(w) = weak.upgrade() {
            update_rows(&w, |rows| {
                if index >= 0 && (index as usize) < rows.len() {
                    rows.remove(index as usize);
                }
            });
        }
    });
    let weak = window.as_weak();
    window.on_move_jump(move |from, to| {
        if let Some(w) = weak.upgrade() {
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
