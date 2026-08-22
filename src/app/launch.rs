#[cfg(test)]
#[path = "../../tests/app/window_management/launch_intent.rs"]
mod launch_intent_tests;

/// What this process launch is supposed to do, parsed from argv before any
/// window exists. `--new-window` is issued by the OS entry points (Windows
/// jump list, macOS dock menu, Linux desktop action); when an instance is
/// already running it is forwarded over the single-instance socket instead
/// of opening a second process.
pub struct LaunchIntent {
    pub new_window: bool,
}

pub fn parse(args: &[String]) -> LaunchIntent {
    LaunchIntent {
        new_window: args.iter().any(|a| a == "--new-window"),
    }
}
