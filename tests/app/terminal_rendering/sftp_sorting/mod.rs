use super::*;

fn sftp_entry(name: &str, is_dir: bool) -> SftpEntry {
    SftpEntry {
        name: name.into(),
        full_path: format!("/{name}").into(),
        is_dir,
        size: String::new().into(),
        size_bytes: 0.0,
        modified: String::new().into(),
        modified_ts: 0.0,
        mode: 0,
        file_type: if is_dir { "directory" } else { "file" }.into(),
        permissions: String::new().into(),
        permissions_mode: 0,
        owner: String::new().into(),
        group: String::new().into(),
        selected: false,
    }
}

fn sftp_names(entries: &[SftpEntry]) -> Vec<String> {
    entries.iter().map(|e| e.name.to_string()).collect()
}

#[test]
fn sftp_name_sort_uses_natural_numeric_order() {
    let mut entries = vec![
        sftp_entry("file100", false),
        sftp_entry("file10", false),
        sftp_entry("file2", false),
        sftp_entry("file11", false),
        sftp_entry("file1", false),
    ];
    sort_sftp_entries(&mut entries, "name", 1);
    assert_eq!(
        sftp_names(&entries),
        vec!["file1", "file2", "file10", "file11", "file100"]
    );

    sort_sftp_entries(&mut entries, "name", -1);
    assert_eq!(
        sftp_names(&entries),
        vec!["file100", "file11", "file10", "file2", "file1"]
    );
}

#[test]
fn sftp_default_sort_keeps_dirs_first_with_natural_names() {
    let mut entries = vec![
        sftp_entry("file100", false),
        sftp_entry("dir10", true),
        sftp_entry("file11", false),
        sftp_entry("dir2", true),
    ];
    sort_sftp_entries(&mut entries, "", 0);
    assert_eq!(
        sftp_names(&entries),
        vec!["dir2", "dir10", "file11", "file100"]
    );
}

#[test]
fn sftp_metadata_columns_sort_without_breaking_directory_grouping() {
    let mut entries = vec![
        {
            let mut e = sftp_entry("z-file", false);
            e.permissions = "-rw-------".into();
            e.permissions_mode = 0o100600;
            e
        },
        {
            let mut e = sftp_entry("a-dir", true);
            e.permissions = "drwxr-xr-x".into();
            e.permissions_mode = 0o40755;
            e
        },
        {
            let mut e = sftp_entry("a-file", false);
            e.permissions = "-rwx------".into();
            e.permissions_mode = 0o100700;
            e
        },
    ];
    sort_sftp_entries(&mut entries, "permissions", 1);
    assert_eq!(sftp_names(&entries), vec!["a-dir", "z-file", "a-file"]);
}

fn checked(model: &VecModel<SftpEntry>) -> Vec<usize> {
    (0..model.row_count())
        .filter(|&i| model.row_data(i).map(|e| e.selected).unwrap_or(false))
        .collect()
}

#[test]
fn sftp_shift_click_range_selects_exactly_the_span() {
    let model = VecModel::from(
        (0..6)
            .map(|i| sftp_entry(&format!("f{i}"), false))
            .collect::<Vec<_>>(),
    );
    // Pre-existing check outside the range gets cleared.
    let mut e = model.row_data(5).unwrap();
    e.selected = true;
    model.set_row_data(5, e);

    assert_eq!(apply_range_selection(&model, 1, 3), 3);
    assert_eq!(checked(&model), vec![1, 2, 3]);

    // Reverse direction (anchor below the clicked row).
    assert_eq!(apply_range_selection(&model, 4, 2), 3);
    assert_eq!(checked(&model), vec![2, 3, 4]);

    // Out-of-range indices are clamped.
    assert_eq!(apply_range_selection(&model, -3, 99), 6);
    assert_eq!(checked(&model), vec![0, 1, 2, 3, 4, 5]);
}

#[test]
fn sftp_select_all_checks_every_row_then_clears_them() {
    // #566: the header's select-all box and Ctrl+A.
    let model = VecModel::from(
        (0..4)
            .map(|i| sftp_entry(&format!("f{i}"), i == 2))
            .collect::<Vec<_>>(),
    );
    let mut e = model.row_data(1).unwrap();
    e.selected = true;
    model.set_row_data(1, e);

    assert_eq!(set_all_selection(&model, true), 4);
    assert_eq!(checked(&model), vec![0, 1, 2, 3]);

    assert_eq!(set_all_selection(&model, false), 0);
    assert!(checked(&model).is_empty());

    let empty: VecModel<SftpEntry> = VecModel::default();
    assert_eq!(set_all_selection(&empty, true), 0);
}
