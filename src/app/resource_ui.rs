use super::*;

pub(super) fn push_ring(buf: &mut Vec<f32>, val: f32) {
    if buf.len() != NET_HISTORY_LEN {
        *buf = vec![0.0; NET_HISTORY_LEN];
    }
    buf.remove(0);
    buf.push(val);
}

/// Auto-scale a raw bytes/sec history to 0..1 against its own window peak so the
/// sparkline always uses the full height (like FinalShell's relative graph).
pub(super) fn normalized_model(buf: &[f32]) -> ModelRc<f32> {
    let max = buf.iter().cloned().fold(1.0_f32, f32::max);
    let scaled: Vec<f32> = buf.iter().map(|v| (v / max).clamp(0.0, 1.0)).collect();
    ModelRc::from(Rc::new(VecModel::from(scaled)))
}

fn is_noise_mount(mount: &str) -> bool {
    let m = mount.trim();
    m == "/dev"
        || m.starts_with("/dev/")
        || m == "/run"
        || m.starts_with("/run/")
        || m == "/sys"
        || m.starts_with("/sys/")
        || m == "/proc"
        || m.starts_with("/proc/")
        || m.starts_with("/snap/")
        || m.contains("/docker/containers/")
        || m.contains("/docker/overlay2/")
        || m.contains("/kubelet/pods/")
        || m.contains("/containerd/")
}

/// Build the filesystem-usage model (path, "avail/total", used fraction),
/// filtering out pseudo/container filesystems (/dev, /run, docker containers).
pub(super) fn disk_rows(disks: &[(String, u64, u64)]) -> Vec<DiskInfo> {
    let has_real_mounts = disks.iter().any(|(m, _, _)| !is_noise_mount(m));
    disks
        .iter()
        .filter(|(m, _, _)| !has_real_mounts || !is_noise_mount(m))
        .map(|(mount, avail, total)| {
            let used = total.saturating_sub(*avail);
            let percent = if *total > 0 {
                used as f32 / *total as f32
            } else {
                0.0
            };
            DiskInfo {
                path: mount.clone().into(),
                detail: format!("{}/{}", format_size(*avail), format_size(*total)).into(),
                percent,
            }
        })
        .collect()
}

pub(super) fn disk_model(disks: &[(String, u64, u64)]) -> ModelRc<DiskInfo> {
    ModelRc::from(Rc::new(VecModel::from(disk_rows(disks))))
}

/// Build the process-monitor model for the popup (#23). `cpu`/`mem` are
/// pre-formatted to one decimal; `cpu_frac` (0..1) drives the row's load bar.
pub(super) fn set_process_action_error(weak: &slint::Weak<ProcWindow>, message: &str) {
    if let Some(window) = weak.upgrade() {
        window.set_action_busy(false);
        window.set_action_error(true);
        window.set_action_status(message.into());
    }
}

/// A root login can signal any process directly. Non-root logins may signal
/// only their own processes; root and other users' processes require `su`.
pub(super) fn process_needs_root(current_user: &str, process_user: &str) -> bool {
    current_user != "root" && process_user != current_user
}

pub(super) fn proc_rows(procs: &[ProcInfo], current_user: &str, tab_id: &str) -> Vec<ProcRow> {
    procs
        .iter()
        .map(|p| ProcRow {
            tab_id: tab_id.into(),
            pid: p.pid.to_string().into(),
            user: p.user.clone().into(),
            cpu: format!("{:.1}", p.cpu).into(),
            mem: format!("{:.1}", p.mem).into(),
            command: p.command.clone().into(),
            cpu_frac: (p.cpu / 100.0).clamp(0.0, 1.0),
            own_process: !process_needs_root(current_user, &p.user),
        })
        .collect()
}

#[cfg(test)]
#[path = "../../tests/app/process_monitor/mod.rs"]
mod process_row_tests;

pub(super) fn metric_rows(
    cpu: f32,
    mem: f32,
    swap: f32,
    mem_detail: impl Into<SharedString>,
    swap_detail: impl Into<SharedString>,
) -> Vec<SysMetricRow> {
    vec![
        SysMetricRow {
            label: "CPU".into(),
            percent: cpu,
            detail: "".into(),
            kind: 0,
        },
        SysMetricRow {
            label: t("内存", "Memory").into(),
            percent: mem,
            detail: mem_detail.into(),
            kind: 1,
        },
        SysMetricRow {
            label: t("交换", "Swap").into(),
            percent: swap,
            detail: swap_detail.into(),
            kind: 2,
        },
    ]
}

pub(super) fn net_rows(net: &[(String, u64, u64)]) -> Vec<SysNetRow> {
    net.iter()
        .map(|(name, rx, tx)| SysNetRow {
            name: name.clone().into(),
            up: format_bytes_per_sec(*tx).into(),
            down: format_bytes_per_sec(*rx).into(),
        })
        .collect()
}

pub(super) fn pairs_to_overview_rows(pairs: &[(String, String)]) -> Vec<SysInfoRow> {
    pairs
        .chunks(2)
        .map(|chunk| {
            let first = &chunk[0];
            let second = chunk.get(1);
            SysInfoRow {
                c1: first.0.clone().into(),
                c2: first.1.clone().into(),
                c3: second.map(|p| p.0.clone()).unwrap_or_default().into(),
                c4: second.map(|p| p.1.clone()).unwrap_or_default().into(),
                c5: "".into(),
            }
        })
        .collect()
}

pub(super) fn pairs_to_one_row(pairs: &[(String, String)]) -> Vec<SysInfoRow> {
    let value = |idx: usize| {
        pairs
            .get(idx)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| "-".to_string())
    };
    vec![SysInfoRow {
        c1: value(0).into(),
        c2: value(1).into(),
        c3: value(2).into(),
        c4: value(3).into(),
        c5: value(4).into(),
    }]
}

pub(super) fn pairs_to_rows(pairs: &[(String, String)], width: usize) -> Vec<SysInfoRow> {
    pairs
        .chunks(width)
        .filter(|chunk| {
            chunk
                .iter()
                .any(|(_, v)| !v.trim().is_empty() && v.trim() != "-")
        })
        .map(|chunk| {
            let value = |idx: usize| {
                chunk
                    .get(idx)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_else(|| "-".to_string())
            };
            SysInfoRow {
                c1: value(0).into(),
                c2: value(1).into(),
                c3: value(2).into(),
                c4: value(3).into(),
                c5: value(4).into(),
            }
        })
        .collect()
}

pub(super) fn cpu_usage_detail_rows(pairs: &[(String, String)]) -> Vec<SysInfoRow> {
    let value = |idx: usize| {
        pairs
            .get(idx)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| "0.0%".to_string())
    };
    let extra = pairs
        .iter()
        .skip(4)
        .map(|(k, v)| format!("{k} {v}"))
        .collect::<Vec<_>>()
        .join(" / ");
    vec![SysInfoRow {
        c1: value(0).into(),
        c2: value(2).into(),
        c3: value(1).into(),
        c4: value(3).into(),
        c5: extra.into(),
    }]
}

pub(super) fn tuple5_rows(rows: &[(String, String, String, String, String)]) -> Vec<SysInfoRow> {
    rows.iter()
        .map(|r| SysInfoRow {
            c1: r.0.clone().into(),
            c2: r.1.clone().into(),
            c3: r.2.clone().into(),
            c4: r.3.clone().into(),
            c5: r.4.clone().into(),
        })
        .collect()
}

/// Mirror the main window's theme/scale/UI-font onto the detached process
/// window. Theme is a per-window Slint global, so a detached window keeps its
/// compile-time (dark) defaults until we copy these across (#23).
pub(super) fn sync_proc_theme(main: &AppWindow, proc: &ProcWindow) {
    proc.set_dark_mode(main.get_dark_mode());
    proc.set_ui_scale(main.get_ui_scale());
    proc.set_ui_font_family(main.get_ui_font_family());
    // Mirror the immersive wallpaper so the detached window shares the frosted
    // backdrop instead of a flat panel.
    proc.set_wallpaper_img(main.get_wallpaper_img());
    proc.set_wallpaper_active(main.get_wallpaper_active());
    proc.set_wp_accent(main.get_wp_accent());
    proc.set_wp_tint(main.get_wp_tint());
    proc.set_theme_palette_dark(main.get_theme_palette_dark());
    proc.set_theme_palette_light(main.get_theme_palette_light());
    proc.set_immersive_colors(main.get_immersive_colors());
}

pub(super) fn sync_system_info_theme(main: &AppWindow, sys: &SystemInfoWindow) {
    sys.set_dark_mode(main.get_dark_mode());
    sys.set_ui_scale(main.get_ui_scale());
    sys.set_ui_font_family(main.get_ui_font_family());
    sys.set_lang_en(main.get_lang_en());
    sys.set_wallpaper_img(main.get_wallpaper_img());
    sys.set_wallpaper_active(main.get_wallpaper_active());
    sys.set_wp_accent(main.get_wp_accent());
    sys.set_wp_tint(main.get_wp_tint());
    sys.set_theme_palette_dark(main.get_theme_palette_dark());
    sys.set_theme_palette_light(main.get_theme_palette_light());
    sys.set_immersive_colors(main.get_immersive_colors());
}

pub(super) fn sync_editor_theme(main: &AppWindow, editor: &EditorWindow) {
    editor.set_dark_mode(main.get_dark_mode());
    editor.set_ui_scale(main.get_ui_scale());
    editor.set_ui_font_family(main.get_ui_font_family());
    editor.set_wallpaper_img(main.get_wallpaper_img());
    editor.set_wallpaper_active(main.get_wallpaper_active());
    editor.set_wp_accent(main.get_wp_accent());
    editor.set_wp_tint(main.get_wp_tint());
    editor.set_theme_palette_dark(main.get_theme_palette_dark());
    editor.set_theme_palette_light(main.get_theme_palette_light());
    editor.set_immersive_colors(main.get_immersive_colors());
}

pub(super) fn place_system_info_window(main: &AppWindow, sys: &SystemInfoWindow) {
    let Some((mon_x, mon_y, mon_w, mon_h)) = main
        .window()
        .with_winit_window(|ww| {
            let scale = ww.scale_factor().max(0.01);
            let monitor = ww.current_monitor().or_else(|| ww.primary_monitor())?;
            let pos = monitor.position();
            let size = monitor.size();
            Some((
                pos.x as f64 / scale,
                pos.y as f64 / scale,
                size.width as f64 / scale,
                size.height as f64 / scale,
            ))
        })
        .flatten()
    else {
        return;
    };

    let target_w = (mon_w * 0.5).clamp(760.0, (mon_w - 24.0).max(760.0));
    let target_h = (mon_h * 0.5).clamp(520.0, (mon_h - 24.0).max(520.0));
    let x = mon_x + (mon_w - target_w).max(0.0) / 2.0;
    let y = mon_y + (mon_h - target_h).max(0.0) / 2.0;

    // Use the Slint window API instead of the winit handle: hidden windows are
    // no longer materialized eagerly (they map on Wayland and pollute the
    // taskbar — see the vendor patch in i-slint-backend-winit), so the first
    // open may run before the native window exists. In that state the Slint
    // API stores the size/position on the adapter and applies it at creation;
    // with a live window it behaves like the winit calls did.
    sys.window()
        .set_size(slint::LogicalSize::new(target_w as f32, target_h as f32));
    sys.window()
        .set_position(slint::LogicalPosition::new(x as f32, y as f32));
}

/// Center the process monitor on the same physical monitor as the main window.
/// Physical coordinates avoid logical/physical rounding errors when the two
/// displays use different DPI scale factors. Keep the user's current process
/// window size; opening it should reposition, not reset a manual resize.
pub(super) fn place_process_window(main: &AppWindow, process: &ProcWindow) {
    let monitor = main
        .window()
        .with_winit_window(|ww| ww.current_monitor().or_else(|| ww.primary_monitor()))
        .flatten();
    let Some(monitor) = monitor else { return };
    let origin = monitor.position();
    let monitor_size = monitor.size();

    // The winit window may not exist yet on the first open (deferred creation,
    // see place_system_info_window); fall back to a zero size, which anchors
    // the window's top-left at the monitor center until the next open.
    // Wayland ignores client-side positioning entirely, so this only affects
    // X11/Windows first-open placement.
    let window_size = process
        .window()
        .with_winit_window(|ww| ww.outer_size())
        .unwrap_or_default();
    let x = origin.x + monitor_size.width.saturating_sub(window_size.width) as i32 / 2;
    let y = origin.y + monitor_size.height.saturating_sub(window_size.height) as i32 / 2;
    process
        .window()
        .set_position(slint::PhysicalPosition::new(x, y));
}
