use super::*;
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

thread_local! {
    static AVAILABLE: Cell<bool> = const { Cell::new(false) };
}

pub(super) fn available() -> bool {
    AVAILABLE.with(Cell::get)
}

pub(super) fn clear() {
    AVAILABLE.with(|state| state.set(false));
}

struct TrayController {
    _icon: TrayIcon,
    open: MenuId,
    new_window: MenuId,
    quit: MenuId,
}

fn create() -> anyhow::Result<TrayController> {
    let image = image::load_from_memory(include_bytes!("../../assets/icon@512.png"))?.into_rgba8();
    let small = image::imageops::resize(&image, 32, 32, image::imageops::FilterType::Lanczos3);
    let icon = tray_icon::Icon::from_rgba(small.into_raw(), 32, 32)?;
    let open = MenuItem::new(t("打开主窗口", "Open Window"), true, None);
    let new_window = MenuItem::new(t("新建窗口", "New Window"), true, None);
    let quit = MenuItem::new(t("退出", "Quit"), true, None);
    let menu = Menu::with_items(&[&open, &new_window, &quit])?;
    let ids = (
        open.id().clone(),
        new_window.id().clone(),
        quit.id().clone(),
    );
    let tray = TrayIconBuilder::new()
        .with_icon(icon)
        .with_tooltip("MeatShell")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()?;
    Ok(TrayController {
        _icon: tray,
        open: ids.0,
        new_window: ids.1,
        quit: ids.2,
    })
}

fn restore_window(core: &Rc<AppCore>) {
    let windows: Vec<_> = core
        .window_states
        .borrow()
        .values()
        .map(|state| state.main_win.clone())
        .collect();
    for window in &windows {
        if !window.window().is_visible() {
            if let Err(error) = window.show() {
                tracing::warn!("failed to restore window from tray: {error}");
            }
        }
    }
    if let Some(weak) = core.registry.newest() {
        if let Some(window) = weak.upgrade() {
            raise_to_front(&window);
            return;
        }
    }
    if let Err(error) = open_window(core.clone(), false, None) {
        tracing::warn!("failed to open window from tray: {error:#}");
    }
}

fn quit(core: &Rc<AppCore>) {
    let windows: Vec<_> = core
        .window_states
        .borrow()
        .values()
        .map(|state| state.main_win.clone())
        .collect();
    if windows.is_empty() {
        let _ = slint::quit_event_loop();
    }
    for window in windows {
        window.invoke_confirm_close_yes();
    }
}

pub(super) fn hide_window(core: &Rc<AppCore>, window_id: u64, window: &AppWindow) {
    let state = core.window_states.borrow().get(&window_id).cloned();
    if let Some(state) = state {
        let _ = state.proc_win.hide();
        let _ = state.sys_win.hide();
        let _ = state.editor_win.hide();
    }
    window.set_process_window_open(false);
    window.set_system_info_window_open(false);
    window.set_editor_open(false);
    let _ = window.hide();
}

/// Poll on Slint's thread, where the Windows/macOS tray implementation and
/// application windows are both owned. The first tick runs inside the event loop.
pub(super) fn install(core: Rc<AppCore>) -> slint::Timer {
    let timer = slint::Timer::default();
    let mut tray: Option<TrayController> = None;
    let mut attempted = false;
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(100),
        move || {
            if !attempted {
                attempted = true;
                match create() {
                    Ok(value) => {
                        tray = Some(value);
                        AVAILABLE.with(|state| state.set(true));
                    }
                    Err(error) => tracing::warn!("system tray unavailable: {error:#}"),
                }
            }
            let Some(tray) = tray.as_ref() else {
                return;
            };
            while let Ok(event) = MenuEvent::receiver().try_recv() {
                if event.id == tray.open {
                    restore_window(&core);
                } else if event.id == tray.new_window {
                    if let Err(error) = open_window(core.clone(), true, None) {
                        tracing::warn!("failed to create window from tray: {error:#}");
                    }
                } else if event.id == tray.quit {
                    quit(&core);
                }
            }
            while let Ok(event) = TrayIconEvent::receiver().try_recv() {
                if matches!(
                    event,
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    }
                ) {
                    restore_window(&core);
                }
            }
        },
    );
    timer
}
