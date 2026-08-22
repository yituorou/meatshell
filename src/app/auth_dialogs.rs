use super::*;

pub(super) fn hostkey_dialog_text(
    host: &str,
    port: u16,
    key_type: &str,
    fingerprint: &str,
    changed: bool,
) -> (String, String, String, String) {
    let detail = format!("{host}:{port}  ({key_type})\n{fingerprint}");
    if changed {
        (
            crate::i18n::t("⚠ 主机密钥已改变", "⚠ Host key changed").to_string(),
            crate::i18n::t(
                "该主机的密钥与之前记录的不一致,可能存在中间人攻击。仅当你确知服务器密钥已更换时才继续。",
                "This host's key differs from the one stored earlier — this could be a man-in-the-middle attack. Only continue if you know the server's key really changed.",
            )
            .to_string(),
            detail,
            crate::i18n::t("仍然信任", "Trust anyway").to_string(),
        )
    } else {
        (
            crate::i18n::t("未知主机", "Unknown host").to_string(),
            crate::i18n::t(
                "首次连接该主机。请核对下面的密钥指纹,确认无误后再信任并连接。",
                "First time connecting to this host. Verify the key fingerprint below before you trust and connect.",
            )
            .to_string(),
            detail,
            crate::i18n::t("信任并连接", "Trust & connect").to_string(),
        )
    }
}

/// Queue a host-key prompt: answer immediately if already decided this run,
/// merge into an existing pending entry for the same host *in the same
/// window*, otherwise enqueue (and show it in the owning window if that
/// window has no other host-key dialog up). Prompts from different windows
/// never merge — each window decides for itself (#multi-window).
#[allow(clippy::too_many_arguments)] // window_id tags the owning window
pub(super) fn enqueue_hostkey_prompt(
    win: &AppWindow,
    window_id: u64,
    host: String,
    port: u16,
    key_type: String,
    fingerprint: String,
    changed: bool,
    responder: crate::ssh::HostKeyResponder,
) {
    let id = format!("{host}:{port}");
    if let Some(ans) = HOSTKEY_DECIDED.with(|d| d.borrow().get(&id).copied()) {
        responder.respond(ans);
        return;
    }
    let show_now = HOSTKEY_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        if let Some(p) = q
            .iter_mut()
            .find(|p| p.window_id == window_id && p.host == host && p.port == port)
        {
            p.responders.push(responder);
            return false;
        }
        // Show only if this window has no other queued prompt of this type:
        // each window's oldest queued entry is the one displayed in its
        // dialog, so a second prompt must wait its turn in *that* window.
        let show_now = !q.iter().any(|p| p.window_id == window_id);
        let (title, message, detail, confirm_label) =
            hostkey_dialog_text(&host, port, &key_type, &fingerprint, changed);
        q.push_back(PendingHostKey {
            window_id,
            host,
            port,
            changed,
            title,
            message,
            detail,
            confirm_label,
            responders: vec![responder],
        });
        show_now
    });
    if show_now {
        show_front_hostkey(win, window_id);
    }
}

/// Push this window's oldest pending prompt's details into the window and
/// open the dialog.
pub(super) fn show_front_hostkey(win: &AppWindow, window_id: u64) {
    HOSTKEY_QUEUE.with(|q| {
        if let Some(p) = q.borrow().iter().find(|p| p.window_id == window_id) {
            win.set_hostkey_changed(p.changed);
            win.set_hostkey_title(p.title.clone().into());
            win.set_hostkey_message(p.message.clone().into());
            win.set_hostkey_detail(p.detail.clone().into());
            win.set_hostkey_confirm_label(p.confirm_label.clone().into());
            win.set_hostkey_prompt_open(true);
        }
    });
}

/// Apply the user's decision to this window's oldest pending prompt, then
/// show that window's next prompt (or close the dialog if it has none left).
pub(super) fn resolve_front_hostkey(win: &AppWindow, window_id: u64, accept: bool) {
    let has_next = HOSTKEY_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        if let Some(pos) = q.iter().position(|p| p.window_id == window_id) {
            let p = q.remove(pos).expect("position checked above");
            // Only remember an *accept* for this run (so a slightly-later SFTP
            // prompt for the same host is answered without a second dialog). We
            // must NOT cache a reject: a single dismissal — e.g. an accidental
            // backdrop click instead of "Trust & connect" — used to poison the
            // host for the whole session, auto-rejecting every later connect with
            // "Unknown server key" until the app was restarted (#152). A reject now
            // only fails the current attempt; the next connect prompts again.
            if accept {
                HOSTKEY_DECIDED.with(|d| {
                    d.borrow_mut()
                        .insert(format!("{}:{}", p.host, p.port), true);
                });
            }
            for r in &p.responders {
                r.respond(accept);
            }
        }
        q.iter().any(|p| p.window_id == window_id)
    });
    if has_next {
        show_front_hostkey(win, window_id);
    } else {
        win.set_hostkey_prompt_open(false);
    }
}

/// Abort every queued prompt owned by `window_id` (called when that window
/// closes): answer each with reject/cancel so the blocked connection attempts
/// fail cleanly instead of hanging forever on a dialog that will never show.
pub(super) fn abort_window_prompts(window_id: u64) {
    HOSTKEY_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        let mut i = 0;
        while i < q.len() {
            if q[i].window_id == window_id {
                let p = q.remove(i).expect("index checked above");
                for r in &p.responders {
                    r.respond(false);
                }
            } else {
                i += 1;
            }
        }
    });
    CRED_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        let mut i = 0;
        while i < q.len() {
            if q[i].window_id == window_id {
                let p = q.remove(i).expect("index checked above");
                for r in &p.responders {
                    r.respond(None);
                }
            } else {
                i += 1;
            }
        }
    });
    MFA_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        let mut i = 0;
        while i < q.len() {
            if q[i].window_id == window_id {
                let p = q.remove(i).expect("index checked above");
                for r in &p.responders {
                    r.respond(None);
                }
            } else {
                i += 1;
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Connect-time credential prompt (#110)
// ---------------------------------------------------------------------------

thread_local! {
    static CRED_QUEUE: RefCell<VecDeque<PendingCred>> = RefCell::new(VecDeque::new());
    /// session id → the answer given this run (`None` = cancelled), so a second
    /// connection for the same session is answered without re-prompting.
    static CRED_DECIDED: RefCell<HashMap<String, Option<crate::ssh::CredentialReply>>> =
        RefCell::new(HashMap::new());
}

/// Queue a credential prompt: answer immediately if already decided this run,
/// merge into an existing pending entry for the same session *in the same
/// window*, otherwise enqueue (and show it in the owning window if that
/// window has no other credential dialog up) (#multi-window).
#[allow(clippy::too_many_arguments)] // window_id tags the owning window
pub(super) fn enqueue_cred_prompt(
    win: &AppWindow,
    window_id: u64,
    session_id: String,
    host: String,
    user: String,
    need_user: bool,
    need_password: bool,
    responder: crate::ssh::CredentialResponder,
) {
    if let Some(reply) = CRED_DECIDED.with(|d| d.borrow().get(&session_id).cloned()) {
        responder.respond(reply);
        return;
    }
    let show_now = CRED_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        if let Some(p) = q
            .iter_mut()
            .find(|p| p.window_id == window_id && p.session_id == session_id)
        {
            p.responders.push(responder);
            return false;
        }
        // Per-window turn: show only if this window has no other queued
        // credential prompt (its oldest entry is the one on screen).
        let show_now = !q.iter().any(|p| p.window_id == window_id);
        q.push_back(PendingCred {
            window_id,
            session_id,
            host,
            user,
            need_user,
            need_password,
            responders: vec![responder],
        });
        show_now
    });
    if show_now {
        show_front_cred(win, window_id);
    }
}

/// Populate the credential dialog from this window's oldest pending prompt and
/// open it.
pub(super) fn show_front_cred(win: &AppWindow, window_id: u64) {
    CRED_QUEUE.with(|q| {
        if let Some(p) = q.borrow().iter().find(|p| p.window_id == window_id) {
            win.set_cred_host(p.host.clone().into());
            win.set_cred_need_user(p.need_user);
            win.set_cred_need_password(p.need_password);
            win.set_cred_user(p.user.clone().into());
            win.set_cred_password("".into());
            win.set_cred_remember(false);
            win.set_cred_prompt_open(true);
        }
    });
}

/// Apply the user's answer to this window's oldest credential prompt (or
/// cancel), persist it when "remember" is checked, then show that window's
/// next prompt or close its dialog.
pub(super) fn resolve_front_cred(win: &AppWindow, window_id: u64, accept: bool) {
    let reply: Option<crate::ssh::CredentialReply> = if accept {
        Some((
            win.get_cred_user().to_string(),
            win.get_cred_password().to_string(),
            win.get_cred_remember(),
        ))
    } else {
        None
    };
    let has_next = CRED_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        if let Some(pos) = q.iter().position(|p| p.window_id == window_id) {
            let p = q.remove(pos).expect("position checked above");
            CRED_DECIDED.with(|d| {
                d.borrow_mut().insert(p.session_id.clone(), reply.clone());
            });
            if let Some((ref u, ref pw, true)) = reply {
                persist_credentials(&p.session_id, u, pw, p.need_user, p.need_password);
            }
            for r in &p.responders {
                r.respond(reply.clone());
            }
        }
        q.iter().any(|p| p.window_id == window_id)
    });
    // Don't leave the typed password lingering in the UI property.
    win.set_cred_password("".into());
    if has_next {
        show_front_cred(win, window_id);
    } else {
        win.set_cred_prompt_open(false);
    }
}

/// Persist newly-entered credentials onto the saved session (#110, "remember").
pub(super) fn persist_credentials(
    session_id: &str,
    user: &str,
    password: &str,
    set_user: bool,
    set_password: bool,
) {
    HISTORY_STORE.with(|s| {
        if let Some(store) = s.borrow().as_ref() {
            let mut st = store.borrow_mut();
            if let Some(mut sess) = st.get(session_id).cloned() {
                if set_user && !user.trim().is_empty() {
                    sess.user = user.trim().to_string();
                }
                if set_password {
                    sess.password = crate::config::Secret::new(password.to_string());
                }
                st.upsert(sess);
                let _ = st.save();
            }
        }
    });
}

// ---------------------------------------------------------------------------
// MFA / keyboard-interactive prompt (#86-MFA)
// ---------------------------------------------------------------------------

thread_local! {
    static MFA_QUEUE: RefCell<VecDeque<PendingMfa>> = RefCell::new(VecDeque::new());
}

/// Queue an MFA prompt: a concurrent connection for the same session *in the
/// same window* (the shell and its SFTP channel both hitting the prompt at
/// once) merges into the open dialog so the code is only typed once; otherwise
/// enqueue (and show it in the owning window if that window has no other MFA
/// dialog up). We deliberately do NOT cache answers across attempts — a wrong
/// code must re-prompt on reconnect, not be silently replayed. Cross-window
/// prompts never merge — each window types its own code (#multi-window).
pub(super) fn enqueue_mfa_prompt(
    win: &AppWindow,
    window_id: u64,
    session_id: String,
    host: String,
    prompt: String,
    echo: bool,
    responder: crate::ssh::MfaResponder,
) {
    let show_now = MFA_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        if let Some(p) = q
            .iter_mut()
            .find(|p| p.window_id == window_id && p.session_id == session_id)
        {
            p.responders.push(responder);
            return false;
        }
        // Per-window turn: show only if this window has no other queued MFA
        // prompt (its oldest entry is the one on screen).
        let show_now = !q.iter().any(|p| p.window_id == window_id);
        q.push_back(PendingMfa {
            window_id,
            session_id,
            host,
            prompt,
            echo,
            responders: vec![responder],
        });
        show_now
    });
    if show_now {
        show_front_mfa(win, window_id);
    }
}

/// Populate the MFA dialog from this window's oldest pending prompt and open
/// it.
pub(super) fn show_front_mfa(win: &AppWindow, window_id: u64) {
    MFA_QUEUE.with(|q| {
        if let Some(p) = q.borrow().iter().find(|p| p.window_id == window_id) {
            win.set_mfa_host(p.host.clone().into());
            win.set_mfa_prompt(p.prompt.clone().into());
            win.set_mfa_echo(p.echo);
            win.set_mfa_answer("".into());
            win.set_mfa_prompt_open(true);
        }
    });
}

/// Apply the user's answer to this window's oldest MFA prompt (or cancel),
/// then show that window's next prompt or close its dialog.
pub(super) fn resolve_front_mfa(win: &AppWindow, window_id: u64, accept: bool) {
    let answer: Option<String> = if accept {
        Some(win.get_mfa_answer().to_string())
    } else {
        None
    };
    let has_next = MFA_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        if let Some(pos) = q.iter().position(|p| p.window_id == window_id) {
            let p = q.remove(pos).expect("position checked above");
            for r in &p.responders {
                r.respond(answer.clone());
            }
        }
        q.iter().any(|p| p.window_id == window_id)
    });
    // Don't leave the typed code lingering in the UI property.
    win.set_mfa_answer("".into());
    if has_next {
        show_front_mfa(win, window_id);
    } else {
        win.set_mfa_prompt_open(false);
    }
}

// ---------------------------------------------------------------------------
// Split panes (v0.5)
// ---------------------------------------------------------------------------
