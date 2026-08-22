//! macOS Dock menu: right-click the Dock icon → "新建窗口".
//!
//! AppKit asks the application delegate for `applicationDockMenu:`; while
//! winit's delegate is installed it does not fall back to `NSApplication`
//! itself (verified on macOS — patching `NSApplication` left the Dock menu
//! default). We therefore add two methods to the delegate's class at
//! runtime with `class_addMethod` (falling back to the `NSApplication`
//! class only when no delegate exists yet):
//!
//! * `applicationDockMenu:` — builds and returns the Dock menu (a single
//!   "新建窗口" entry). The menu is constructed fresh on every request and
//!   autoreleased, matching AppKit's expectation that the returned menu is
//!   not owned by the Dock.
//! * `meatshellNewWindow:` — the menu item's action (target = the receiver
//!   of `applicationDockMenu:`, i.e. the same object whose class we
//!   patched). It routes into `crate::app::request_new_window()`, which
//!   opens a new window through the hook `run()` installs.
//!
//! Threading: the Dock menu is only requested on the main thread, which is
//! also Slint's UI thread in this app, so the action handler can open the
//! window directly without `invoke_from_event_loop`.

use std::ffi::c_char;

use objc2::ffi::class_addMethod;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{msg_send, sel, ClassType, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem};
use objc2_foundation::NSString;

/// `applicationDockMenu:` implementation: `fn(id self, SEL _cmd, id sender) -> NSMenu *`.
type DockMenuFn = unsafe extern "C-unwind" fn(&AnyObject, Sel, &AnyObject) -> *mut NSMenu;
/// `meatshellNewWindow:` implementation: `fn(id self, SEL _cmd, id sender) -> void`.
type NewWindowFn = unsafe extern "C-unwind" fn(&AnyObject, Sel, &AnyObject);

/// Add `applicationDockMenu:` and `meatshellNewWindow:` where AppKit will
/// actually look for them. AppKit asks the application DELEGATE for
/// `applicationDockMenu:` first, and while winit's delegate is installed it
/// never falls back to the `NSApplication` class (verified on macOS:
/// patching `NSApplication` left the Dock menu default). So patch the
/// delegate's class; only if no delegate exists yet fall back to
/// `NSApplication`. Call this after the first window is created, which is
/// when winit has set up the delegate. Failures are logged and swallowed —
/// a missing Dock entry must never keep the app from starting.
pub fn install_dock_menu() {
    // Defensive: class patching happens once at startup on the main thread,
    // where `run()` executes. Bail out instead of touching AppKit off-main.
    let Some(mtm) = MainThreadMarker::new() else {
        tracing::warn!("dock menu install skipped: not on the main thread");
        return;
    };

    unsafe {
        let app = NSApplication::sharedApplication(mtm);
        // Raw runtime lookup: AppKit's -[NSApplication delegate], then the
        // delegate instance's class. Going through msg_send sidesteps the
        // ProtocolObject type gymnastics entirely.
        let delegate: *mut AnyObject = msg_send![&app, delegate];
        let cls: *mut AnyClass = if delegate.is_null() {
            (NSApplication::class() as *const AnyClass).cast_mut()
        } else {
            msg_send![delegate, class]
        };

        // SAFETY: `Imp` is an argument-less `unsafe extern "C-unwind" fn()`;
        // the runtime passes self/_cmd/sender per the type encoding given to
        // `class_addMethod`, which matches each implementation's real
        // signature (see the type aliases above). Transmuting between
        // same-ABI function pointers is the standard way to register IMPs.
        let dock_imp: Imp = std::mem::transmute::<DockMenuFn, Imp>(dock_menu_impl);
        let action_imp: Imp = std::mem::transmute::<NewWindowFn, Imp>(new_window_impl);

        // - (NSMenu *)applicationDockMenu:(id)sender;  → "@@:@"
        let dock_ok = class_addMethod(
            cls,
            sel!(applicationDockMenu:),
            dock_imp,
            b"@@:@\0".as_ptr().cast::<c_char>(),
        );
        // - (void)meatshellNewWindow:(id)sender;  → "v@@:@"
        let action_ok = class_addMethod(
            cls,
            sel!(meatshellNewWindow:),
            action_imp,
            b"v@@:@\0".as_ptr().cast::<c_char>(),
        );
        if !dock_ok.as_bool() || !action_ok.as_bool() {
            tracing::warn!(
                dock = dock_ok.as_bool(),
                action = action_ok.as_bool(),
                "dock menu install: class_addMethod failed"
            );
        }
    }
}

/// `applicationDockMenu:` — build the menu fresh on every request and hand
/// AppKit an autoreleased (non-owned) reference. Dock menu requests are rare
/// (one per right-click), so rebuilding avoids keeping a menu alive in a
/// static and any ownership puzzles.
///
/// # Safety
///
/// Invoked by AppKit via the Objective-C runtime on the main thread with a
/// valid `NSApplication` receiver and selector.
unsafe extern "C-unwind" fn dock_menu_impl(
    this: &AnyObject,
    _cmd: Sel,
    _sender: &AnyObject,
) -> *mut NSMenu {
    let Some(mtm) = MainThreadMarker::new() else {
        return std::ptr::null_mut();
    };

    let menu = NSMenu::new(mtm);
    let item = NSMenuItem::initWithTitle_action_keyEquivalent(
        NSMenuItem::alloc(mtm),
        &NSString::from_str("新建窗口"),
        Some(sel!(meatshellNewWindow:)),
        &NSString::new(),
    );
    // Target the NSApplication instance that received applicationDockMenu:.
    item.setTarget(Some(this));
    menu.addItem(&item);

    Retained::autorelease_ptr(menu)
}

/// `meatshellNewWindow:` — the menu item action. Runs on the main thread, so
/// it can open the window directly through the hook installed by `run()`.
///
/// # Safety
///
/// Invoked by AppKit via the Objective-C runtime on the main thread.
unsafe extern "C-unwind" fn new_window_impl(_this: &AnyObject, _cmd: Sel, _sender: &AnyObject) {
    crate::app::request_new_window();
}
