//! Small fixture compiling the production prompt queues without the full desktop.
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::ComponentHandle;
use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    rc::Rc,
};

#[path = "../src/session/struct/pending_prompts.rs"]
mod pending;
#[path = "../src/ssh/struct/responders.rs"]
mod ssh;
use pending::*;
#[path = "../src/app/auth_dialogs.rs"]
mod auth_dialogs;
use auth_dialogs::*;

mod i18n {
    pub fn t<'a>(_: &'a str, english: &'a str) -> &'a str {
        english
    }
}
// Persistence is deliberately inert: these queue tests never load user config.
mod config {
    #[derive(Clone, Default)]
    pub struct Secret;
    impl Secret {
        pub fn new(_: String) -> Self {
            Self
        }
    }
}
#[derive(Clone, Default)]
struct Session {
    user: String,
    password: config::Secret,
}
struct ConfigStore;
impl ConfigStore {
    fn get(&self, _: &str) -> Option<&Session> {
        None
    }
    fn upsert(&mut self, _: Session) {}
    fn save(&self) -> Result<(), ()> {
        Ok(())
    }
}
thread_local! {
    static HISTORY_STORE: RefCell<Option<Rc<RefCell<ConfigStore>>>> = const { RefCell::new(None) };
    static HOSTKEY_QUEUE: RefCell<VecDeque<PendingHostKey>> = RefCell::new(VecDeque::new());
    static HOSTKEY_DECIDED: RefCell<HashMap<String, bool>> = RefCell::new(HashMap::new());
}
slint::slint! {
    export component AppWindow inherits Window {
        in-out property <bool> hostkey-changed;
        in-out property <string> hostkey-title;
        in-out property <string> hostkey-message;
        in-out property <string> hostkey-detail;
        in-out property <string> hostkey-confirm-label;
        in-out property <bool> hostkey-prompt-open;
        in-out property <bool> hostkey-prompt-is-test;
        in-out property <string> cred-host;
        in-out property <string> cred-user;
        in-out property <string> cred-password;
        in-out property <bool> cred-need-user;
        in-out property <bool> cred-need-password;
        in-out property <bool> cred-remember;
        in-out property <bool> cred-prompt-open;
        in-out property <bool> cred-prompt-is-test;
        in-out property <string> mfa-host;
        in-out property <string> mfa-prompt;
        in-out property <string> mfa-answer;
        in-out property <bool> mfa-echo;
        in-out property <bool> mfa-prompt-open;
        in-out property <bool> mfa-prompt-is-test;
    }
}
struct Backend;
impl slint::platform::Platform for Backend {
    fn create_window_adapter(
        &self,
    ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        Ok(MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer))
    }
}
fn with_ui(f: impl FnOnce(AppWindow) + Send + 'static) {
    std::thread::spawn(move || {
        slint::platform::set_platform(Box::new(Backend)).unwrap();
        f(AppWindow::new().unwrap());
    })
    .join()
    .unwrap();
}

#[test]
fn cancelling_test_keeps_real_hostkey_and_other_windows_prompts() {
    with_ui(|w| {
        let (test_tx, mut test_rx) = tokio::sync::oneshot::channel();
        let (real_tx, mut real_rx) = tokio::sync::oneshot::channel();
        let (other_tx, mut other_rx) = tokio::sync::oneshot::channel();
        enqueue_hostkey_prompt_scoped(
            &w,
            1,
            Some(7),
            "same".into(),
            22,
            "key".into(),
            "fp".into(),
            false,
            ssh::HostKeyResponder::new(test_tx),
        );
        enqueue_hostkey_prompt(
            &w,
            1,
            "same".into(),
            22,
            "key".into(),
            "fp".into(),
            false,
            ssh::HostKeyResponder::new(real_tx),
        );
        enqueue_hostkey_prompt_scoped(
            &w,
            2,
            Some(7),
            "same".into(),
            22,
            "key".into(),
            "fp".into(),
            false,
            ssh::HostKeyResponder::new(other_tx),
        );
        assert!(w.get_hostkey_prompt_is_test());
        abort_test_prompts(&w, 1, 7);
        assert_eq!(test_rx.try_recv(), Ok(false));
        assert!(real_rx.try_recv().is_err());
        assert!(other_rx.try_recv().is_err());
        assert!(w.get_hostkey_prompt_open());
        assert!(!w.get_hostkey_prompt_is_test());
        resolve_front_hostkey(&w, 1, true);
        assert_eq!(real_rx.try_recv(), Ok(true));
        abort_window_prompts(2);
        assert_eq!(other_rx.try_recv(), Ok(false));
    });
}

#[test]
fn cancelling_queued_test_does_not_reset_real_credentials_or_mfa() {
    with_ui(|w| {
        let (real_tx, mut real_rx) = tokio::sync::oneshot::channel();
        let (test_tx, mut test_rx) = tokio::sync::oneshot::channel();
        enqueue_cred_prompt(
            &w,
            1,
            "same".into(),
            "host".into(),
            "user".into(),
            false,
            true,
            ssh::CredentialResponder::new(real_tx),
        );
        enqueue_cred_prompt_scoped(
            &w,
            1,
            Some(10),
            "same".into(),
            "host".into(),
            "user".into(),
            false,
            true,
            ssh::CredentialResponder::new(test_tx),
        );
        w.set_cred_password("user-is-still-typing".into());
        let (mfa_tx, mut mfa_rx) = tokio::sync::oneshot::channel();
        let (test_mfa_tx, mut test_mfa_rx) = tokio::sync::oneshot::channel();
        enqueue_mfa_prompt(
            &w,
            1,
            "same".into(),
            "host".into(),
            "Code".into(),
            false,
            ssh::MfaResponder::new(mfa_tx),
        );
        enqueue_mfa_prompt_scoped(
            &w,
            1,
            Some(10),
            "same".into(),
            "host".into(),
            "Code".into(),
            false,
            ssh::MfaResponder::new(test_mfa_tx),
        );
        w.set_mfa_answer("fixture-code".into());
        abort_test_prompts(&w, 1, 10);
        assert_eq!(test_rx.try_recv(), Ok(None));
        assert_eq!(test_mfa_rx.try_recv(), Ok(None));
        assert_eq!(w.get_cred_password(), "user-is-still-typing");
        assert_eq!(w.get_mfa_answer(), "fixture-code");
        assert!(w.get_cred_prompt_open());
        assert!(w.get_mfa_prompt_open());
        assert!(real_rx.try_recv().is_err());
        assert!(mfa_rx.try_recv().is_err());
        abort_window_prompts(1);
    });
}

#[test]
fn cancelled_test_credentials_do_not_poison_next_attempt() {
    with_ui(|w| {
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        enqueue_cred_prompt_scoped(
            &w,
            1,
            Some(10),
            "same".into(),
            "host".into(),
            "user".into(),
            false,
            true,
            ssh::CredentialResponder::new(tx),
        );
        resolve_front_cred(&w, 1, false);
        assert_eq!(rx.try_recv(), Ok(None));
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        enqueue_cred_prompt_scoped(
            &w,
            1,
            Some(11),
            "same".into(),
            "host".into(),
            "user".into(),
            false,
            true,
            ssh::CredentialResponder::new(tx),
        );
        assert!(rx.try_recv().is_err());
        assert!(w.get_cred_prompt_is_test());
        abort_test_prompts(&w, 1, 10); // late old-test cleanup cannot cancel the new one
        assert!(rx.try_recv().is_err());
        abort_test_prompts(&w, 1, 11);
        assert_eq!(rx.try_recv(), Ok(None));
        assert!(!w.get_cred_prompt_open());
    });
}
