fn main() {
    // Some MinGW-w64 distributions (winlibs' "MCF" builds, which thread through
    // mcfgthread) need the mcfgthread import library on the link line, otherwise
    // libgcc_eh.a's emutls.o is left with undefined `_MCF_*` symbols and the
    // link fails. The flag is only added when the library actually exists, so
    // MSVC and winpthreads-based toolchains stay untouched.
    #[cfg(all(windows, target_env = "gnu"))]
    {
        let target = std::env::var("TARGET").unwrap_or_default();
        let arch = target.split('-').next().unwrap_or("x86_64").to_string();
        // The MinGW driver is named after the *host* (x86_64-w64-mingw32-gcc …),
        // not after Rust's own triple (x86_64-pc-windows-gnu), so try both.
        let compilers = [
            format!("{arch}-w64-mingw32-gcc"),
            format!("{target}-gcc"),
            "gcc".to_string(),
        ];
        for compiler in compilers {
            let Ok(out) = std::process::Command::new(&compiler)
                .arg("-print-file-name=libmcfgthread.a")
                .output()
            else {
                continue;
            };
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            // `-print-file-name` echoes the name unchanged when nothing matched.
            if std::path::Path::new(&path).is_file() {
                println!("cargo:rustc-link-arg=-lmcfgthread");
                break;
            }
        }
    }
    // Bundle the gettext `.po` translations under `lang/` so the UI's `@tr(...)`
    // strings can switch language at runtime via slint::select_bundled_translation.
    // Source language is Chinese (the msgids); `lang/<lc>/LC_MESSAGES/meatshell.po`
    // provides other locales.  No per-component context, so msgids are the raw
    // Chinese strings.
    println!("cargo:rerun-if-changed=lang");
    slint_build::compile_with_config(
        "ui/app.slint",
        slint_build::CompilerConfiguration::new()
            .with_style("fluent".into())
            .with_bundled_translations("lang")
            .with_default_translation_context(slint_build::DefaultTranslationContext::None),
    )
    .expect("Slint build failed");

    // Embed the application icon into the Windows executable so it shows up in
    // Explorer, the taskbar and shortcuts. No-op on non-Windows targets.
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=assets/meatshell.ico");
        println!("cargo:rerun-if-changed=assets/meatshell.exe.manifest");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/meatshell.ico");
        // Embed an application manifest declaring Per-Monitor DPI Awareness V2.
        // Without it the DPI-awareness level depends on winit's runtime
        // SetProcessDpiAwarenessContext call, which races: if anything touches a
        // DPI API first the call silently fails and the window jumps in size /
        // cursor offset when dragged across monitors with different scaling (#194).
        // The manifest is authoritative and applied before any code runs.
        res.set_manifest_file("assets/meatshell.exe.manifest");
        if let Err(e) = res.compile() {
            println!("cargo:warning=failed to embed Windows icon: {e}");
        }
    }
}
