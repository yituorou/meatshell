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
    assert_eq!(resolve_path(&display_path(&base), "child"), child);
    // ".." climbs back out, and extra separators / dots collapse.
    let nested = child.join(".").join("..").join("child");
    assert_eq!(normalize(&nested), child);
    // A relative path from the current folder can climb too.
    assert_eq!(resolve_path(&display_path(&child), ".."), base);

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
    assert_eq!(resolve_path("/nowhere", "~"), home);
}

#[test]
fn resolving_never_touches_the_filesystem() {
    // Resolution is pure: a path that does not exist (or a dead network drive)
    // must come back immediately; the listing thread reports the failure.
    let base = temp_dir("pure-resolve");
    let missing = resolve_path(&display_path(&base), "does-not-exist");
    assert_eq!(missing, base.join("does-not-exist"));
    assert!(list_dir(&missing, &ListOptions::default()).is_err());

    // A file path resolves too; listing it is what fails.
    let file = base.join("note.txt");
    std::fs::write(&file, b"hi").unwrap();
    assert_eq!(resolve_path(&display_path(&base), "note.txt"), file);
    assert!(list_dir(&file, &ListOptions::default()).is_err());

    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn listing_puts_folders_first_and_describes_entries() {
    let base = temp_dir("listing");
    std::fs::create_dir_all(base.join("z-folder")).unwrap();
    std::fs::write(base.join("b.txt"), b"12345").unwrap();
    std::fs::write(base.join("a.txt"), b"123").unwrap();

    let listing = list_dir(&base, &ListOptions::default()).unwrap();
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
fn copying_into_its_own_subfolder_is_refused() {
    // Dropping a folder onto a pane that is showing one of its subfolders would
    // otherwise copy the growing target into itself forever.
    let base = temp_dir("self-copy");
    let source = base.join("source");
    std::fs::create_dir_all(source.join("inner")).unwrap();
    std::fs::write(source.join("a.txt"), b"a").unwrap();

    let inside = source.join("inner").join("source");
    let error = copy_recursive(&source, &inside).unwrap_err();
    assert!(
        error.contains("子目录") || error.to_lowercase().contains("subfolder"),
        "{error}"
    );
    assert!(!inside.exists(), "nothing may be written for a refused copy");

    std::fs::remove_dir_all(&base).unwrap();
}

#[cfg(unix)]
#[test]
fn copying_a_symlink_loop_terminates() {
    let base = temp_dir("loop");
    let source = base.join("source");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("a.txt"), b"a").unwrap();
    // A link back to its own folder is an infinite tree for a naive copy.
    std::os::unix::fs::symlink(&source, source.join("loop")).unwrap();

    let target = base.join("target");
    copy_recursive(&source, &target).unwrap();
    assert_eq!(std::fs::read(target.join("a.txt")).unwrap(), b"a");
    assert!(
        !target.join("loop").join("loop").exists(),
        "the link must not be descended into again"
    );

    std::fs::remove_dir_all(&base).unwrap();
}

#[cfg(windows)]
#[test]
fn copying_a_junction_loop_terminates() {
    let base = temp_dir("junction");
    let source = base.join("source");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("a.txt"), b"a").unwrap();

    // A junction needs no administrator rights (unlike a symlink) and is the
    // loop users actually hit: `mklink /J loop source`.
    let link = source.join("loop");
    let created = {
        use std::os::windows::process::CommandExt as _;
        let mut command = std::process::Command::new("cmd");
        // `raw_arg`: cmd.exe does not undo C-runtime quoting, so a quoted
        // argument would reach mklink with literal backslashes.
        command.arg("/c").raw_arg(format!(
            "mklink /J \"{}\" \"{}\"",
            link.display(),
            source.display()
        ));
        command
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    };
    if !created || !link.exists() {
        eprintln!("skipping: cannot create a junction here");
        std::fs::remove_dir_all(&base).ok();
        return;
    }

    let target = base.join("target");
    copy_recursive(&source, &target).unwrap();
    assert_eq!(std::fs::read(target.join("a.txt")).unwrap(), b"a");
    assert!(
        !target.join("loop").join("loop").exists(),
        "the junction must not be descended into again"
    );

    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn very_deep_trees_stop_at_the_depth_limit() {
    // Backstop for link loops `canonicalize` cannot collapse.
    let base = temp_dir("deep");
    let source = base.join("source");
    let mut deep = source.clone();
    for _ in 0..(MAX_COPY_DEPTH as usize + 5) {
        deep = deep.join("d");
        std::fs::create_dir_all(&deep).unwrap();
    }

    let target = base.join("target");
    assert!(copy_recursive(&source, &target).is_err());

    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn entry_cap_is_documented_by_a_constant() {
    // Guards against a silent change making listings unbounded again.
    assert!(MAX_LOCAL_ENTRIES >= 1_000);
}

#[test]
fn filter_matches_names_case_insensitively() {
    let base = temp_dir("filter");
    std::fs::create_dir_all(base.join("Src")).unwrap();
    std::fs::write(base.join("notes.txt"), b"a").unwrap();
    std::fs::write(base.join("build.log"), b"b").unwrap();

    let listing = list_dir(&base, &ListOptions::default()).unwrap();
    let names = |needle: &str| -> Vec<String> {
        filter_entries(&listing.entries, needle)
            .into_iter()
            .map(|entry| entry.name.to_string())
            .collect()
    };
    // Empty or whitespace-only filters keep everything.
    assert_eq!(names("").len(), 3);
    assert_eq!(names("   ").len(), 3);
    // Case-insensitive substring, and folders are matched too.
    assert_eq!(names("SRC"), ["Src".to_string()]);
    assert_eq!(names(".TXT"), ["notes.txt".to_string()]);
    assert!(names("nope").is_empty());

    std::fs::remove_dir_all(&base).unwrap();
}

#[cfg(windows)]
#[test]
fn hidden_entries_are_listed_only_when_asked() {
    let base = temp_dir("hidden");
    std::fs::write(base.join("visible.txt"), b"a").unwrap();
    let hidden = base.join("secret.txt");
    std::fs::write(&hidden, b"a").unwrap();
    // `attrib +H` is what Explorer's "hidden" checkbox sets; no extra crate.
    let marked = std::process::Command::new("attrib")
        .arg("+H")
        .arg(&hidden)
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if !marked {
        eprintln!("skipping: cannot set the hidden attribute here");
        std::fs::remove_dir_all(&base).ok();
        return;
    }

    let names = |show_hidden: bool| -> Vec<String> {
        list_dir(
            &base,
            &ListOptions {
                show_hidden,
                ..Default::default()
            },
        )
        .unwrap()
        .entries
        .into_iter()
        .map(|entry| entry.name.to_string())
        .collect()
    };
    assert_eq!(names(false), ["visible.txt".to_string()]);
    assert_eq!(
        names(true),
        ["secret.txt".to_string(), "visible.txt".to_string()]
    );

    std::fs::remove_dir_all(&base).ok();
}

#[cfg(not(windows))]
#[test]
fn dotfiles_are_listed_only_when_asked() {
    let base = temp_dir("dotfiles");
    std::fs::write(base.join("visible.txt"), b"a").unwrap();
    std::fs::write(base.join(".hidden"), b"a").unwrap();

    let plain = list_dir(&base, &ListOptions::default()).unwrap();
    assert_eq!(plain.entries.len(), 1);
    let with_hidden = list_dir(
        &base,
        &ListOptions {
            show_hidden: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(with_hidden.entries.len(), 2);

    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn listing_honours_the_sort_column() {
    let base = temp_dir("sort");
    std::fs::write(base.join("small.txt"), b"1").unwrap();
    std::fs::write(base.join("large.txt"), b"123456").unwrap();

    let opts = |key: &str, dir: i32| ListOptions {
        sort_key: key.to_string(),
        sort_dir: dir,
        ..Default::default()
    };
    let names = |options: &ListOptions| -> Vec<String> {
        list_dir(&base, options)
            .unwrap()
            .entries
            .into_iter()
            .map(|entry| entry.name.to_string())
            .collect()
    };
    // Default: natural name order.
    assert_eq!(names(&ListOptions::default()), ["large.txt", "small.txt"]);
    assert_eq!(names(&opts("size", 1)), ["small.txt", "large.txt"]);
    assert_eq!(names(&opts("size", -1)), ["large.txt", "small.txt"]);

    std::fs::remove_dir_all(&base).unwrap();
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
