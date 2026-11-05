//! Start / stop a tab's session log from the tab context menu (#535).
//!
//! Automatic logging (#265) writes every tab into the configured folder.
//! This lets the user record one open terminal or serial tab on demand into
//! a file they choose, and stop it again, without touching the setting.

use super::*;

/// Print a one-line `[session log]` notice into the tab's terminal.
fn notice(buffer: &mut TermBuffer, colour: &str, text: &str) {
    let line = format!("\r\n\x1b[{colour}m{text}\x1b[0m\r\n");
    let _ = buffer.ingest(line.as_bytes());
}

pub(super) fn wire(window: &AppWindow, store: Rc<RefCell<ConfigStore>>, bufs: TermBuffers) {
    {
        let bufs = bufs.clone();
        window.on_tab_log_active(move |tab_id: SharedString| {
            with_term_buf(&bufs, tab_id.as_str(), |b| b.session_log.is_some()).unwrap_or(false)
        });
    }

    let weak = window.as_weak();
    window.on_tab_log_toggle(move |tab_id: SharedString| {
        let tid = tab_id.to_string();
        let Some((recording, default_name)) = with_term_buf(&bufs, &tid, |b| {
            (b.session_log.is_some(), b.default_session_log_file_name())
        }) else {
            return;
        };

        if recording {
            with_term_buf(&bufs, &tid, |b| {
                if let Some(path) = b.stop_session_log() {
                    tracing::info!("session log: stopped {}", path.display());
                    notice(
                        b,
                        "33",
                        &format!(
                            "{} {}",
                            t("[会话日志] 已停止记录,日志已保存到", "[session log] stopped; saved to"),
                            path.display()
                        ),
                    );
                }
            });
        } else {
            // Tabs without terminal output (RDP) are disabled in the menu.
            let Some(default_name) = default_name else {
                return;
            };
            let dir = store.borrow().session_log_dir();
            let _ = std::fs::create_dir_all(&dir);
            let Some(path) = DialogOwner::of_weak(&weak)
                .file()
                .set_title(t("保存会话日志", "Save session log"))
                .set_directory(&dir)
                .set_file_name(&default_name)
                .add_filter(t("日志文件", "Log files"), &["log", "txt"])
                .save_file()
            else {
                return;
            };
            with_term_buf(&bufs, &tid, |b| match b.start_session_log_at(&path) {
                Ok(()) => {
                    tracing::info!("session log: recording to {}", path.display());
                    notice(
                        b,
                        "32",
                        &format!(
                            "{} {}",
                            t("[会话日志] 开始记录到", "[session log] recording to"),
                            path.display()
                        ),
                    );
                }
                Err(err) => {
                    tracing::warn!("session log: cannot create {}: {err}", path.display());
                    notice(
                        b,
                        "33",
                        &format!(
                            "{} {} ({err})",
                            t("[会话日志] 无法创建日志文件:", "[session log] could not create"),
                            path.display()
                        ),
                    );
                }
            });
        }

        if let Some(w) = weak.upgrade() {
            super::terminal_ui::rebuild_tab_display(&w, &bufs, &tid);
        }
    });
}
