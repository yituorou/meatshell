// Entry point. Wires the Slint UI to the config store, system sampler and
// SSH session manager.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod automation;
mod cli;
mod config;
mod i18n;
mod layout;
mod logging;
mod mcp;
mod resource;
mod session;
mod sftp;
mod ssh;
mod terminal;
mod tunnel;
mod ui;
mod wallpaper;
mod webdav;

enum StartMode {
    Mcp,
    Cli,
    App,
    Version,
}

impl StartMode {
    fn detect(args: &[String]) -> Self {
        match args.get(1).map(String::as_str) {
            Some("mcp") if args.get(2).is_some_and(|arg| arg == "serve") => Self::Mcp,
            Some("cli") => Self::Cli,
            _ if args.iter().any(|arg| arg == "--version" || arg == "-V") => Self::Version,
            _ => Self::App,
        }
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    let mode = StartMode::detect(&args);
    if matches!(mode, StartMode::Version) {
        println!("meatshell {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    init_tracing();

    match mode {
        StartMode::Mcp => mcp::run_stdio(),
        StartMode::Cli => cli::run(&args),
        StartMode::App => {
            // macOS defaults to Slint's CPU renderer. FemtoVG and Skia remain available
            // in Settings -> Interface -> Rendering for users who prefer GPU rendering.
            //
            // History: 0.4.10 force-set SLINT_BACKEND=winit-skia to work around femtovg's
            // CoreText font lookup failing on macOS 26 / Tahoe (all text vanished, #108).
            // That fix shipped without on-device verification and turned out to *break* a
            // different set of Macs (Apple Silicon M5 / 26.5): Skia couldn't resolve the
            // "PingFang SC" UI font and all text vanished there instead (#129). Icons
            // survived in both cases because Material Icons is an embedded font.
            //
            // Neither GPU renderer works for every macOS machine, so software rendering
            // is the compatibility default. Users can select FemtoVG or Skia under
            // Settings -> Interface -> Rendering. The
            // SLINT_BACKEND=winit-skia diagnostic override remains available and takes
            // precedence over the saved setting. The renderer-skia feature is compiled in
            // on macOS (see Cargo.toml), so switching does not require a rebuild.

            // ── IME policy ───────────────────────────────────────────────────────────
            // NOTE: We deliberately DO **NOT** call `ImmDisableIME` here.
            //
            // An earlier version disabled the IME for the whole Slint event-loop thread
            // to work around a vim `:q!` glitch (Chinese IMEs intercept letter keys and,
            // on a Shift press, discard the in-flight pinyin).  But disabling the IME
            // also makes 中文输入 completely impossible — there is no composition window
            // at all, which is exactly the "无法输入任何中文" bug.
            //
            // Chinese input now flows through the hidden `ime-input` TextInput in
            // terminal_view.slint: composition happens there, and committed text is
            // forwarded to the PTY via the `edited` callback.  The vim/Shift side-effects
            // are handled instead by the C0-marker + 3-layer Backspace filters in
            // `app::on_send_key`, so we no longer need (and must not use) ImmDisableIME.
            let intent = app::launch::parse(&args);
            app::run(intent)
        }
        StartMode::Version => unreachable!("handled above"),
    }
}

/// Set up tracing: stderr (honours RUST_LOG, default info) **plus** a capped
/// `error.log` file at WARN and above so users can send diagnostics — e.g. a
/// bastion disconnect reason — without setting RUST_LOG (#86).
fn init_tracing() {
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::{fmt, EnvFilter};

    // Third-party noise routed through `log` → tracing: ICU4X data-error warnings
    // (icu_provider dependency) and fontdb's "malformed font" warning for fonts it
    // can't parse but harmlessly skips (e.g. Windows' mstmc.ttf). Silence on every
    // layer; keep fontdb at `error` so genuine failures still surface.
    fn quiet_noise(mut f: EnvFilter) -> EnvFilter {
        for d in [
            "icu_provider=off",
            "icu_segmenter=off",
            "icu_normalizer=off",
            "fontdb=error",
        ] {
            if let Ok(dir) = d.parse() {
                f = f.add_directive(dir);
            }
        }
        f
    }

    let env_filter =
        quiet_noise(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")));
    let stderr_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(env_filter);

    // One file, capped at 50 MiB, auto-overwriting when full (5 MiB was too
    // small to diagnose anything useful).
    let file_layer = logging::path()
        .and_then(|p| logging::CappedFile::open(p, 50 * 1024 * 1024).ok())
        .map(|cf| {
            fmt::layer()
                .with_ansi(false)
                .with_writer(logging::CappedWriter::new(cf))
                .with_filter(quiet_noise(EnvFilter::new("warn")))
        });

    tracing_subscriber::registry()
        .with(stderr_layer)
        .with(file_layer)
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_start_mode() {
        let mcp = vec![
            "meatshell".to_string(),
            "mcp".to_string(),
            "serve".to_string(),
        ];
        assert!(matches!(StartMode::detect(&mcp), StartMode::Mcp));

        let cli = vec![
            "meatshell".to_string(),
            "cli".to_string(),
            "sessions".to_string(),
        ];
        assert!(matches!(StartMode::detect(&cli), StartMode::Cli));

        let version = vec!["meatshell".to_string(), "--version".to_string()];
        assert!(matches!(StartMode::detect(&version), StartMode::Version));

        let app = vec!["meatshell".to_string()];
        assert!(matches!(StartMode::detect(&app), StartMode::App));
    }
}
