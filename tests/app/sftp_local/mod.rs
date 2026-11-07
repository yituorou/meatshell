use super::*;

/// A unique, empty temp directory for one test.
fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "meatshell-sftp-local-{tag}-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn parent_navigation_joins_and_normalizes() {
    let base = temp_dir("normalize");
    let child = base.join("child");
    std::fs::create_dir_all(&child).unwrap();

    // A relative name resolves against the current folder.
    assert_eq!(resolve_path(&display_path(&base), "child").unwrap(), child);
    // ".." climbs back out, and extra separators / dots collapse.
    let nested = child.join(".").join("..").join("child");
    assert_eq!(normalize(&nested), child);
    // A relative path from the current folder can climb too.
    assert_eq!(resolve_path(&display_path(&child), "..").unwrap(), base);

    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn root_has_no_parent_and_cannot_climb_out() {
    #[cfg(windows)]
    let root = PathBuf::from(r"C:\");
    #[cfg(not(windows))]
    let root = PathBuf::from("/");

    assert!(parent_of(&display_path(&root)).is_none());
    assert_eq!(normalize(&root.join("..")), root);
    assert_eq!(normalize(&root.join("a").join("..").join("..")), root);
}

#[test]
fn tilde_resolves_to_the_home_folder() {
    let home = home_dir();
    assert!(home.is_dir(), "home dir {} must exist", home.display());
    assert_eq!(resolve_path("/nowhere", "~").unwrap(), home);
}

#[test]
fn a_file_is_not_navigable() {
    let base = temp_dir("not-a-dir");
    let file = base.join("note.txt");
    std::fs::write(&file, b"hi").unwrap();

    let error = resolve_path(&display_path(&base), "note.txt").unwrap_err();
    assert!(
        error.contains("不是文件夹") || error.to_lowercase().contains("not a folder"),
        "{error}"
    );

    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn missing_folder_reports_an_error_instead_of_panicking() {
    let base = temp_dir("missing");
    let error = resolve_path(&display_path(&base), "does-not-exist").unwrap_err();
    assert!(!error.is_empty());
    assert!(list_dir(&base.join("does-not-exist")).is_err());
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn listing_puts_folders_first_and_describes_entries() {
    let base = temp_dir("listing");
    std::fs::create_dir_all(base.join("z-folder")).unwrap();
    std::fs::write(base.join("b.txt"), b"12345").unwrap();
    std::fs::write(base.join("a.txt"), b"123").unwrap();

    let listing = list_dir(&base).unwrap();
    assert!(!listing.truncated);
    assert_eq!(listing.path, display_path(&base));
    let names: Vec<&str> = listing.entries.iter().map(|e| e.name.as_str()).collect();
    // Natural order: the folder first, then files alphabetically.
    assert_eq!(names, ["z-folder", "a.txt", "b.txt"]);

    let folder = &listing.entries[0];
    assert!(folder.is_dir);
    assert_eq!(folder.size.as_str(), "");
    // Local rows carry no POSIX metadata; those columns stay blank.
    assert!(folder.permissions.is_empty());
    assert!(folder.owner.is_empty());

    let file = &listing.entries[1];
    assert!(!file.is_dir);
    assert_eq!(file.size_bytes, 3.0);
    assert_eq!(file.full_path.as_str(), display_path(&base.join("a.txt")));
    assert!(file.mode == 0 && file.permissions_mode == 0);

    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn verbatim_windows_prefixes_are_hidden_from_the_path_bar() {
    assert_eq!(display_path(Path::new(r"\\?\C:\Users\me")), r"C:\Users\me");
    assert_eq!(
        display_path(Path::new(r"\\?\UNC\server\share\file")),
        r"\\server\share\file"
    );
    // Ordinary paths pass through untouched.
    assert_eq!(display_path(Path::new(r"C:\Temp")), r"C:\Temp");
}

#[test]
fn home_display_follows_the_platform_separator() {
    let shown = home_display();
    assert!(!shown.is_empty());
    assert!(shown.contains(std::path::MAIN_SEPARATOR));
}

#[path = "render.rs"]
mod render;

#[test]
fn copying_a_folder_tree_preserves_the_contents() {
    let base = temp_dir("copy");
    let source = base.join("source");
    std::fs::create_dir_all(source.join("nested")).unwrap();
    std::fs::write(source.join("a.txt"), b"a").unwrap();
    std::fs::write(source.join("nested").join("b.txt"), b"bb").unwrap();

    let target = base.join("target");
    copy_recursive(&source, &target).unwrap();
    assert_eq!(std::fs::read(target.join("a.txt")).unwrap(), b"a");
    assert_eq!(
        std::fs::read(target.join("nested").join("b.txt")).unwrap(),
        b"bb"
    );

    // A single file copies too, creating its parent folder.
    let copy = base.join("copy").join("deep").join("a.txt");
    copy_recursive(&source.join("a.txt"), &copy).unwrap();
    assert_eq!(std::fs::read(&copy).unwrap(), b"a");

    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn entry_cap_is_documented_by_a_constant() {
    // Guards against a silent change making listings unbounded again.
    assert!(MAX_LOCAL_ENTRIES >= 1_000);
}

#[test]
fn places_list_the_home_folder_and_mark_it_current() {
    let home = home_display();
    let open = quick_places(&home);
    assert!(!open.is_empty(), "home must always be offered");
    assert!(same_dir(&open[0].path, &home), "{:?}", open[0]);
    assert!(open[0].current, "the pane is showing the home folder");

    // A folder that is not one of the places highlights nothing.
    let elsewhere = quick_places(r"C:\definitely\not\a\place");
    assert!(elsewhere.iter().all(|place| !place.current));
    // Every row carries something to render.
    assert!(open.iter().all(|place| !place.path.is_empty() && !place.label.is_empty()));
}

#[cfg(windows)]
#[test]
fn places_offer_whole_drives_only() {
    let drives = drive_places("");
    assert!(!drives.is_empty(), "a Windows machine has at least one drive");
    // Drive roots like `C:\`, never a folder mount point.
    assert!(drives
        .iter()
        .all(|drive| drive.path.len() == 3 && drive.path.ends_with('\\')));
    assert!(drives.iter().any(|drive| drive.path.starts_with('C')));
    // Sorted, so the menu order does not depend on enumeration order.
    let mut sorted: Vec<&str> = drives.iter().map(|drive| drive.path.as_str()).collect();
    sorted.sort();
    assert!(sorted == drives.iter().map(|drive| drive.path.as_str()).collect::<Vec<_>>());
    // Labels fall back to the letter when the volume has no name.
    assert!(drives.iter().all(|drive| drive.label.starts_with(&drive.path[..2])));
}
