use super::*;

#[cfg(windows)]
#[test]
fn local_windows_paths_split_at_the_last_separator() {
    let e = split_local_path(r"C:\Users\", r"C:\");
    assert_eq!(e.dir, r"C:\Users\");
    assert_eq!(e.prefix, r"C:\Users\");
    assert_eq!(e.sep, '\\');
    assert_eq!(e.fragment, "");

    let e = split_local_path(r"C:\Us", r"C:\");
    assert_eq!(e.dir, r"C:\");
    assert_eq!(e.prefix, r"C:\");
    assert_eq!(e.fragment, "Us");

    // Empty field: complete under the folder the pane shows.
    let e = split_local_path("", r"C:\Users");
    assert_eq!(e.dir, r"C:\Users");
    assert_eq!(e.prefix, r"C:\Users\");
    assert_eq!(e.fragment, "");

    let e = split_local_path(r"C:\Users\me\work", r"C:\");
    assert_eq!(e.dir, r"C:\Users\me\");
    assert_eq!(e.fragment, "work");

    // A drive root stays a root, not a drive-relative `C:`.
    let e = split_local_path(r"C:", r"C:\Users");
    assert_eq!(e.dir, r"C:\");
    assert_eq!(e.prefix, r"C:\");
}

#[cfg(not(windows))]
#[test]
fn local_unix_paths_split_at_the_last_slash() {
    let e = split_local_path("/home/alice/", "/");
    assert_eq!(e.dir, "/home/alice/");
    assert_eq!(e.prefix, "/home/alice/");
    assert_eq!(e.sep, '/');
    assert_eq!(e.fragment, "");

    let e = split_local_path("/home/al", "/");
    assert_eq!(e.dir, "/home/");
    assert_eq!(e.prefix, "/home/");
    assert_eq!(e.fragment, "al");

    let e = split_local_path("~/Do", "/home/alice");
    assert_eq!(e.dir, display_path(&home_dir()));
    assert_eq!(e.prefix, "~/");
    assert_eq!(e.fragment, "Do");

    // A relative fragment lists the pane's folder and completes relative to it.
    let e = split_local_path("Doc", "/home/alice");
    assert_eq!(e.dir, "/home/alice");
    assert_eq!(e.prefix, "");
    assert_eq!(e.fragment, "Doc");
}

#[test]
fn remote_paths_split_at_the_last_slash() {
    let e = split_remote_path("/home/", "/").unwrap();
    assert_eq!(e.dir, "/home/");
    assert_eq!(e.prefix, "/home/");
    assert_eq!(e.sep, '/');
    assert_eq!(e.fragment, "");

    let e = split_remote_path("/home/al", "/").unwrap();
    assert_eq!(e.dir, "/home/");
    assert_eq!(e.fragment, "al");

    // Relative input resolves against the folder the pane shows.
    let e = split_remote_path("lo", "/var/log").unwrap();
    assert_eq!(e.dir, "/var/log/");
    assert_eq!(e.prefix, "/var/log/");
    assert_eq!(e.fragment, "lo");

    // The empty field lists the current folder.
    let e = split_remote_path("", "/var/log").unwrap();
    assert_eq!(e.dir, "/var/log/");
    assert_eq!(e.fragment, "");

    // `~` has no SFTP meaning, so there is nothing to list.
    assert!(split_remote_path("~/x", "/").is_none());
}

#[test]
fn suggestions_filter_folders_by_prefix_and_build_completed_paths() {
    let names = vec![
        "usr".to_string(),
        "User".to_string(),
        "var".to_string(),
        ".cache".to_string(),
    ];
    let rows = names_to_suggestions(&names, "/home/", '/', "u");
    assert_eq!(
        rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        ["usr", "User"]
    );
    assert_eq!(rows[0].path.as_str(), "/home/usr/");
    assert_eq!(rows[1].path.as_str(), "/home/User/");

    // Hidden folders only when the fragment asks for them.
    assert!(names_to_suggestions(&names, "/home/", '/', "")
        .iter()
        .all(|r| !r.name.starts_with('.')));
    let hidden = names_to_suggestions(&names, "/home/", '/', ".");
    assert_eq!(hidden.len(), 1);
    assert_eq!(hidden[0].path.as_str(), "/home/.cache/");

    // The dropdown is capped.
    let many: Vec<String> = (0..20).map(|i| format!("d{i}")).collect();
    assert_eq!(
        names_to_suggestions(&many, "/", '/', "d").len(),
        MAX_SUGGESTIONS
    );
}

#[test]
fn local_listing_returns_folders_only_and_sorted() {
    let dir = std::env::temp_dir().join(format!("meatshell-complete-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("beta")).unwrap();
    std::fs::create_dir_all(dir.join("Alpha")).unwrap();
    std::fs::write(dir.join("file.txt"), b"x").unwrap();
    let names = list_local_dirs(&dir.to_string_lossy());
    assert_eq!(names, ["Alpha", "beta"]);
    let _ = std::fs::remove_dir_all(&dir);
}
