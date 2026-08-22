//! Windows taskbar jump list: right-click the taskbar icon → "新建窗口",
//! launching `meatshell --new-window`. The launch is forwarded to the
//! running primary instance over the single-instance IPC socket (see
//! `single_instance.rs`), so the entry behaves like Chrome's "new window"
//! task instead of spawning a second process.
//!
//! Registration runs at startup, before the first window is shown, and must
//! never block or fail startup: every error path degrades to a tracing warn.

use windows::core::{w, Interface, HRESULT, HSTRING, PROPVARIANT};
use windows::Win32::Storage::EnhancedStorage::{PKEY_AppUserModel_ID, PKEY_Title};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::Common::{IObjectArray, IObjectCollection};
use windows::Win32::UI::Shell::{
    DestinationList, EnumerableObjectCollection, ICustomDestinationList, IShellLinkW,
    PropertiesSystem::IPropertyStore, SetCurrentProcessExplicitAppUserModelID, ShellLink,
};

/// AppUserModelID used to attach the jump list to this application.
/// Must stay in sync with the identity Explorer uses for meatshell.
const APP_ID: &str = "meatshell";

/// Register the "新建窗口" user task on the taskbar jump list.
///
/// Failures are logged and swallowed — a missing jump list entry must never
/// keep the app from starting.
pub fn register_new_window_task() {
    set_app_user_model_id();
    if let Err(e) = register_inner() {
        tracing::warn!("jump list registration failed: {e}");
    }
}

/// Declare the process AppUserModelID so Explorer associates this process
/// with the jump list published under `APP_ID` (taskbar grouping and the
/// custom destination list). Without it the shell derives an ID from the
/// exe path and may silently ignore our list.
///
/// The `w!` literal must equal `APP_ID` (the macro needs a string literal).
/// Failure is warn-only, like everything else in this module.
pub fn set_app_user_model_id() {
    if let Err(e) = unsafe { SetCurrentProcessExplicitAppUserModelID(w!("meatshell")) } {
        tracing::warn!("SetCurrentProcessExplicitAppUserModelID failed: {e}");
    }
}

fn register_inner() -> windows::core::Result<()> {
    unsafe {
        // The main thread has no COM apartment yet at this point in startup;
        // ignore S_FALSE (already initialized) and RPC_E_CHANGED_MODE (some
        // shell component initialized it differently) — either way COM is
        // usable from here.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

        // current_exe() also works when launched as a portable/AppImage-style
        // bare binary, so the jump list keeps pointing at the running image.
        let exe = std::env::current_exe().map_err(|e| {
            windows::core::Error::new(
                HRESULT(e.raw_os_error().unwrap_or(0)),
                format!("current_exe: {e}"),
            )
        })?;

        // Shell link: target = this exe, args = --new-window.
        let exe_h = HSTRING::from(exe.as_path());
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_ALL)
            .map_err(step("CoCreateInstance(ShellLink)"))?;
        link.SetPath(&exe_h).map_err(step("SetPath"))?;
        link.SetArguments(w!("--new-window"))
            .map_err(step("SetArguments"))?;
        link.SetDescription(w!("新建窗口"))
            .map_err(step("SetDescription"))?;
        link.SetIconLocation(&exe_h, 0)
            .map_err(step("SetIconLocation"))?;

        // Windows 11 validates user-task links strictly: without an explicit
        // PKEY_Title the shell rejects the task with E_INVALIDARG from
        // AddUserTasks (SetDescription does not count as a display name).
        // AppUserModelID keeps the task grouped with our jump list.
        let store: IPropertyStore = link.cast().map_err(step("cast IPropertyStore"))?;
        store
            .SetValue(&PKEY_Title, &PROPVARIANT::from("新建窗口"))
            .map_err(step("SetValue(Title)"))?;
        store
            .SetValue(&PKEY_AppUserModel_ID, &PROPVARIANT::from(APP_ID))
            .map_err(step("SetValue(AppUserModelID)"))?;
        store.Commit().map_err(step("PropertyStore.Commit"))?;

        // User tasks collection holding the single link.
        let collection: IObjectCollection =
            CoCreateInstance(&EnumerableObjectCollection, None, CLSCTX_ALL)
                .map_err(step("CoCreateInstance(EnumerableObjectCollection)"))?;
        collection.AddObject(&link).map_err(step("AddObject"))?;

        // Custom destination list: publish the task under our AppUserModelID.
        let list: ICustomDestinationList = CoCreateInstance(&DestinationList, None, CLSCTX_ALL)
            .map_err(step("CoCreateInstance(DestinationList)"))?;
        let _ = list.SetAppID(&HSTRING::from(APP_ID));
        let mut slots = 0u32;
        let _removed: IObjectArray = list.BeginList(&mut slots).map_err(step("BeginList"))?;
        list.AddUserTasks(&collection)
            .map_err(step("AddUserTasks"))?;
        list.CommitList().map_err(step("CommitList"))?;
        Ok(())
    }
}

/// Tag a COM failure with the failing step so the log names the exact call.
fn step(name: &'static str) -> impl FnOnce(windows::core::Error) -> windows::core::Error {
    move |e| {
        windows::core::Error::new(
            e.code(),
            format!("{name}: {e} (0x{:08X})", e.code().0 as u32),
        )
    }
}
