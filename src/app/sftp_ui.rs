use super::*;

pub(super) fn terminal_sftp_paths(w: &AppWindow) -> HashMap<String, String> {
    use slint::Model as _;
    let mut out = HashMap::new();
    let model = w.get_terminals();
    if let Some(terminals) = model.as_any().downcast_ref::<VecModel<TerminalState>>() {
        for i in 0..terminals.row_count() {
            if let Some(row) = terminals.row_data(i) {
                out.insert(row.id.to_string(), row.sftp_path.to_string());
            }
        }
    }
    out
}

pub(super) fn sorted_sftp_entries_from_model(
    model: &ModelRc<SftpEntry>,
    key: &str,
    dir: i32,
) -> ModelRc<SftpEntry> {
    let Some(vec_model) = model.as_any().downcast_ref::<VecModel<SftpEntry>>() else {
        return model.clone();
    };
    let mut entries = Vec::with_capacity(vec_model.row_count());
    for i in 0..vec_model.row_count() {
        if let Some(entry) = vec_model.row_data(i) {
            entries.push(entry);
        }
    }
    sort_sftp_entries(&mut entries, key, dir);
    ModelRc::from(std::rc::Rc::new(VecModel::from(entries)))
}

pub(super) fn sort_sftp_entries(entries: &mut [SftpEntry], key: &str, dir: i32) {
    use std::cmp::Ordering;

    let name_cmp = |a: &SftpEntry, b: &SftpEntry| natural_name_cmp(&a.name, &b.name);
    let default_cmp = |a: &SftpEntry, b: &SftpEntry| match (a.is_dir, b.is_dir) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        _ => name_cmp(a, b),
    };

    if dir == 0 || key.is_empty() {
        entries.sort_by(default_cmp);
        return;
    }

    entries.sort_by(|a, b| {
        let group = match (a.is_dir, b.is_dir) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => Ordering::Equal,
        };
        if group != Ordering::Equal {
            return group;
        }
        let ord = match key {
            "size" => a
                .size_bytes
                .partial_cmp(&b.size_bytes)
                .unwrap_or(Ordering::Equal)
                .then_with(|| default_cmp(a, b)),
            "modified" => a
                .modified_ts
                .partial_cmp(&b.modified_ts)
                .unwrap_or(Ordering::Equal)
                .then_with(|| default_cmp(a, b)),
            "permissions" => a
                .permissions_mode
                .cmp(&b.permissions_mode)
                .then_with(|| default_cmp(a, b)),
            "owner" => a.owner.cmp(&b.owner).then_with(|| default_cmp(a, b)),
            "group" => a.group.cmp(&b.group).then_with(|| default_cmp(a, b)),
            "type" => a
                .file_type
                .cmp(&b.file_type)
                .then_with(|| default_cmp(a, b)),
            _ => name_cmp(a, b).then_with(|| default_cmp(a, b)),
        };
        if dir < 0 {
            ord.reverse()
        } else {
            ord
        }
    });
}

pub(super) fn friendly_file_type(name: &str, is_dir: bool, raw_kind: &str) -> SharedString {
    match raw_kind {
        "directory" => return crate::i18n::t("文件夹", "Folder").into(),
        "symlink" => return crate::i18n::t("符号链接", "Symbolic link").into(),
        "socket" => return crate::i18n::t("套接字", "Socket").into(),
        "block-device" => return crate::i18n::t("块设备", "Block device").into(),
        "character-device" => return crate::i18n::t("字符设备", "Character device").into(),
        "fifo" => return crate::i18n::t("管道", "FIFO").into(),
        _ if is_dir => return crate::i18n::t("文件夹", "Folder").into(),
        _ => {}
    }
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        return crate::i18n::t("TAR.GZ 压缩包", "TAR.GZ Archive").into();
    }
    if lower.ends_with(".tar.xz") || lower.ends_with(".txz") {
        return crate::i18n::t("TAR.XZ 压缩包", "TAR.XZ Archive").into();
    }
    let trimmed = name.strip_prefix('.').unwrap_or(name);
    if let Some((_, ext)) = trimmed.rsplit_once('.') {
        let ext = ext.trim();
        if !ext.is_empty() && ext.len() <= 10 && ext.chars().all(|c| c.is_ascii_alphanumeric()) {
            let up = ext.to_ascii_uppercase();
            return format!("{} {}", up, crate::i18n::t("文件", "File")).into();
        }
    }
    if name.starts_with('.') && name.len() > 1 {
        return crate::i18n::t("配置文件", "Dotfile").into();
    }
    crate::i18n::t("文件", "File").into()
}

pub(super) fn sftp_subfolder_matches(
    current_dir: &str,
    typed_path: &str,
    folder_name: &str,
    show_hidden: bool,
) -> bool {
    let cur = current_dir.trim().trim_end_matches(['/', '\\']);
    let typed = typed_path.trim();
    let prefix = if typed.is_empty() || typed.trim_end_matches(['/', '\\']) == cur {
        ""
    } else if !cur.is_empty() && typed.starts_with(cur) {
        let rest = &typed[cur.len()..];
        let Some(after_sep) = rest.strip_prefix('/').or_else(|| rest.strip_prefix('\\')) else {
            return false;
        };
        if after_sep.contains('/') || after_sep.contains('\\') {
            return false;
        }
        after_sep
    } else if cur.is_empty() && (typed.starts_with('/') || typed.starts_with('\\')) {
        let after_sep = typed.trim_start_matches(['/', '\\']);
        if after_sep.contains('/') || after_sep.contains('\\') {
            return false;
        }
        after_sep
    } else if !typed.contains('/') && !typed.contains('\\') {
        typed
    } else {
        return false;
    };

    if !show_hidden && folder_name.starts_with('.') && !prefix.starts_with('.') {
        return false;
    }
    if prefix.is_empty() {
        return true;
    }
    folder_name.to_lowercase().contains(&prefix.to_lowercase())
}

pub(super) fn sftp_entry_matches_query(name: &str, file_type: &str, query: &str) -> bool {
    let (hide_dot, raw_q) = match query.strip_prefix("__hide_dot__:") {
        Some(rest) => (true, rest),
        None => (false, query),
    };
    if let Some(rest) = raw_q.strip_prefix("__sub__:") {
        let (cur_dir, typed) = rest.split_once('\n').unwrap_or((rest, ""));
        return sftp_subfolder_matches(cur_dir, typed, name, !hide_dot);
    }
    if hide_dot && name.starts_with('.') {
        return false;
    }
    let q = raw_q.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    name.to_lowercase().contains(&q) || file_type.to_lowercase().contains(&q)
}

pub(super) fn natural_name_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    natural_ascii_cmp(&a.to_lowercase(), &b.to_lowercase()).then_with(|| a.cmp(b))
}

pub(super) fn natural_ascii_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let ab = a.as_bytes();
    let bb = b.as_bytes();
    let mut ai = 0;
    let mut bi = 0;
    while ai < ab.len() && bi < bb.len() {
        let ad = ab[ai].is_ascii_digit();
        let bd = bb[bi].is_ascii_digit();
        if ad && bd {
            let a_start = ai;
            let b_start = bi;
            while ai < ab.len() && ab[ai].is_ascii_digit() {
                ai += 1;
            }
            while bi < bb.len() && bb[bi].is_ascii_digit() {
                bi += 1;
            }

            let mut a_sig = a_start;
            let mut b_sig = b_start;
            while a_sig < ai && ab[a_sig] == b'0' {
                a_sig += 1;
            }
            while b_sig < bi && bb[b_sig] == b'0' {
                b_sig += 1;
            }

            let a_len = ai - a_sig;
            let b_len = bi - b_sig;
            let ord = a_len
                .cmp(&b_len)
                .then_with(|| ab[a_sig..ai].cmp(&bb[b_sig..bi]))
                .then_with(|| (ai - a_start).cmp(&(bi - b_start)));
            if ord != Ordering::Equal {
                return ord;
            }
            continue;
        }

        let ord = ab[ai].cmp(&bb[bi]);
        if ord != Ordering::Equal {
            return ord;
        }
        ai += 1;
        bi += 1;
    }
    ab.len().cmp(&bb.len())
}

/// Collect the remote paths currently selected in an SFTP tab's file list.
pub(super) fn collect_sftp_selected(
    terminals: &VecModel<TerminalState>,
    tab_id: &str,
) -> Vec<String> {
    let mut paths = Vec::new();
    for ti in 0..terminals.row_count() {
        let Some(row) = terminals.row_data(ti) else {
            continue;
        };
        if row.id.as_str() != tab_id {
            continue;
        }
        if let Some(em) = row
            .sftp_entries
            .as_any()
            .downcast_ref::<VecModel<SftpEntry>>()
        {
            for ei in 0..em.row_count() {
                if let Some(e) = em.row_data(ei) {
                    if e.selected {
                        paths.push(e.full_path.to_string());
                    }
                }
            }
        }
        break;
    }
    paths
}

/// What a drag starting on `row` carries (#504): every checked entry when the
/// row is one of several checked ones, otherwise just that row.
pub(super) fn drag_out_items(
    w: &AppWindow,
    tab_id: &str,
    row: i32,
) -> Vec<super::drag_out::DragItem> {
    let terminals = w.get_terminals();
    let Some(tm) = terminals.as_any().downcast_ref::<VecModel<TerminalState>>() else {
        return Vec::new();
    };
    let Some(term) = (0..tm.row_count())
        .filter_map(|i| tm.row_data(i))
        .find(|t| t.id.as_str() == tab_id)
    else {
        return Vec::new();
    };
    let Some(entry) = usize::try_from(row)
        .ok()
        .and_then(|i| term.sftp_entries.row_data(i))
    else {
        return Vec::new();
    };
    if entry.selected && term.sftp_selected_count > 1 {
        // macOS needs to know folders up front (the drag's file types).
        let dirs: std::collections::HashSet<String> = (0..term.sftp_entries.row_count())
            .filter_map(|i| term.sftp_entries.row_data(i))
            .filter(|e| e.is_dir)
            .map(|e| e.full_path.to_string())
            .collect();
        collect_sftp_selected(tm, tab_id)
            .into_iter()
            .map(|remote| super::drag_out::DragItem {
                is_dir: dirs.contains(&remote),
                remote,
            })
            .collect()
    } else {
        vec![super::drag_out::DragItem {
            remote: entry.full_path.to_string(),
            is_dir: entry.is_dir,
        }]
    }
}

/// Shift+click range selection: make the checked set exactly the rows between
/// `from` and `to` inclusive (either order, clamped to the list). Only rows
/// whose state changes are written back, so large listings don't re-render
/// every row. Returns the resulting checked count.
pub(super) fn apply_range_selection(entries: &VecModel<SftpEntry>, from: i32, to: i32) -> usize {
    let len = entries.row_count();
    if len == 0 {
        return 0;
    }
    let clamp = |v: i32| (v.max(0) as usize).min(len - 1);
    let (lo, hi) = {
        let (a, b) = (clamp(from), clamp(to));
        (a.min(b), a.max(b))
    };
    let mut n = 0;
    for i in 0..len {
        let Some(mut e) = entries.row_data(i) else {
            continue;
        };
        let want = (lo..=hi).contains(&i);
        if want {
            n += 1;
        }
        if e.selected != want {
            e.selected = want;
            entries.set_row_data(i, e);
        }
    }
    n
}

/// Uncheck every SFTP entry for a tab and reset its selected-count (#100).
pub(super) fn clear_sftp_selection(terminals: &VecModel<TerminalState>, tab_id: &str) {
    for ti in 0..terminals.row_count() {
        let Some(row) = terminals.row_data(ti) else {
            continue;
        };
        if row.id.as_str() != tab_id {
            continue;
        }
        if let Some(em) = row
            .sftp_entries
            .as_any()
            .downcast_ref::<VecModel<SftpEntry>>()
        {
            for ei in 0..em.row_count() {
                if let Some(mut e) = em.row_data(ei) {
                    if e.selected {
                        e.selected = false;
                        em.set_row_data(ei, e);
                    }
                }
            }
        }
        let mut r = row.clone();
        r.sftp_selected_count = 0;
        terminals.set_row_data(ti, r);
        break;
    }
}
