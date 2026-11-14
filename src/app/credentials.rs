//! Settings → Credentials (#526): saved SSH logins shared by sessions.
//!
//! A credential holds a username, an authentication method and its secrets.
//! SSH sessions pick one in the session editor instead of keeping their own
//! copy; editing the credential here updates every session that uses it. The
//! secrets never go to the UI: editing leaves the password field blank and a
//! blank password keeps the saved one.

use crate::config::{AuthMethod, ConfigStore, Credential, Secret};
use crate::i18n::t;

use slint::ComponentHandle as _;

/// Push the Settings → Credentials list into one window.
pub(super) fn refresh_settings(w: &super::AppWindow, store: &ConfigStore) {
    let rows: Vec<super::CredentialInfo> = store
        .credentials()
        .iter()
        .map(|credential| super::CredentialInfo {
            id: credential.id.clone().into(),
            name: credential.name.clone().into(),
            summary: credential.summary().into(),
            usage: store.credential_usage(&credential.id) as i32,
        })
        .collect();
    w.set_credentials(slint::ModelRc::new(slint::VecModel::from(rows)));
    // The credential being edited may have been deleted in another window.
    let editing = w.get_cred_edit_id();
    if !editing.is_empty() && store.credential(&editing).is_none() {
        clear_form(w);
    }
}

fn clear_form(w: &super::AppWindow) {
    w.set_cred_edit_id("".into());
    w.set_cred_new_name("".into());
    w.set_cred_new_user("".into());
    w.set_cred_new_auth("password".into());
    w.set_cred_new_password("".into());
    w.set_cred_new_key_path("".into());
    w.set_cred_status("".into());
}

/// Build the credential the form describes. `existing` is the saved version
/// when editing: a blank password keeps its secret, and so does a key that
/// was pasted rather than picked as a file.
fn credential_from_form(
    id: &str,
    name: &str,
    user: &str,
    auth: &str,
    password: &str,
    key_path: &str,
    existing: Option<&Credential>,
) -> Result<Credential, &'static str> {
    let auth = AuthMethod::from_str(auth);
    let key_path = key_path.trim().replace('\\', "/");
    let mut credential = existing.cloned().unwrap_or_else(Credential::new_empty);
    if !id.is_empty() {
        credential.id = id.to_string();
    }
    let auth_changed = existing.is_some_and(|old| old.auth != auth);
    credential.name = name.trim().to_string();
    credential.user = user.trim().to_string();
    credential.auth = auth;
    if !password.is_empty() {
        credential.password = Secret::new(password.to_string());
    } else if auth_changed {
        // A password is not a key passphrase (and vice versa).
        credential.password = Secret::default();
    }
    if auth == AuthMethod::Agent {
        // The agent holds the key (#528); nothing secret is kept here.
        credential.password = Secret::default();
    }
    if auth == AuthMethod::Key {
        if !key_path.is_empty() {
            credential.private_key_path = key_path;
            credential.private_key_inline = Secret::default();
        } else if credential.private_key_inline.is_empty() {
            return Err(t("请选择私钥文件。", "Pick a private key file."));
        }
    }
    if credential.name.is_empty() {
        return Err(t("请填写名称。", "Enter a name."));
    }
    Ok(credential)
}

/// Settings → Credentials callbacks of one window. Lists refresh through the
/// config broadcast → `refresh-agent-launchers`, like custom shells.
pub(super) fn wire(ctx: &super::WinCtx) {
    let window = &ctx.window;
    refresh_settings(window, &ctx.store.borrow());
    {
        let weak = window.as_weak();
        let store = ctx.store.clone();
        let registry = ctx.registry.clone();
        window.on_save_credential(move |id, name, user, auth, password, key_path| {
            let Some(w) = weak.upgrade() else {
                return;
            };
            let result = {
                let mut s = store.borrow_mut();
                let existing = s.credential(&id).cloned();
                if !id.is_empty() && existing.is_none() {
                    Err(t("这个认证已被删除。", "This credential was deleted."))
                } else {
                    credential_from_form(&id, &name, &user, &auth, &password, &key_path, existing.as_ref())
                        .and_then(|credential| {
                            s.upsert_credential(credential);
                            s.save().map_err(|err| {
                                tracing::warn!("failed to save credentials: {err:#}");
                                t("保存失败，请重试。", "Saving failed; please try again.")
                            })
                        })
                }
            };
            match result {
                Ok(()) => {
                    clear_form(&w);
                    registry.broadcast_config_changed();
                }
                Err(message) => w.set_cred_status(message.into()),
            }
        });
    }
    {
        let weak = window.as_weak();
        let store = ctx.store.clone();
        window.on_edit_credential(move |id| {
            let Some(w) = weak.upgrade() else {
                return;
            };
            let s = store.borrow();
            let Some(credential) = s.credential(&id) else {
                return;
            };
            w.set_cred_edit_id(credential.id.clone().into());
            w.set_cred_new_name(credential.name.clone().into());
            w.set_cred_new_user(credential.user.clone().into());
            w.set_cred_new_auth(credential.auth.as_str().into());
            // Never send the secret to the UI; blank keeps it.
            w.set_cred_new_password("".into());
            w.set_cred_new_key_path(credential.private_key_path.clone().into());
            w.set_cred_status("".into());
        });
    }
    {
        let weak = window.as_weak();
        window.on_cancel_credential_edit(move || {
            if let Some(w) = weak.upgrade() {
                clear_form(&w);
            }
        });
    }
    {
        let weak = window.as_weak();
        let store = ctx.store.clone();
        let registry = ctx.registry.clone();
        window.on_remove_credential(move |id| {
            let saved = {
                let mut s = store.borrow_mut();
                s.remove_credential(&id);
                s.save()
            };
            if let Some(w) = weak.upgrade() {
                if w.get_cred_edit_id() == id {
                    clear_form(&w);
                }
                if let Err(err) = saved {
                    tracing::warn!("failed to save credentials: {err:#}");
                    w.set_cred_status(t("保存失败，请重试。", "Saving failed; please try again.").into());
                }
            }
            registry.broadcast_config_changed();
        });
    }
    {
        let weak = window.as_weak();
        window.on_pick_credential_key(move || {
            if let Some(file) = super::DialogOwner::of_weak(&weak).file().pick_file() {
                if let Some(w) = weak.upgrade() {
                    w.set_cred_new_key_path(file.to_string_lossy().to_string().into());
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_password_credential_needs_only_a_name() {
        let c = credential_from_form("", " Lab ", " root ", "password", "pw", "", None).unwrap();
        assert_eq!((c.name.as_str(), c.user.as_str()), ("Lab", "root"));
        assert_eq!(c.password.as_str(), "pw");
        assert!(credential_from_form("", " ", "root", "password", "", "", None).is_err());
    }

    #[test]
    fn editing_with_a_blank_password_keeps_the_saved_one() {
        let saved = credential_from_form("", "Lab", "root", "password", "old", "", None).unwrap();
        let edited =
            credential_from_form(&saved.id, "Lab", "admin", "password", "", "", Some(&saved)).unwrap();
        assert_eq!(edited.id, saved.id);
        assert_eq!(edited.user, "admin");
        assert_eq!(edited.password.as_str(), "old");
        let rotated =
            credential_from_form(&saved.id, "Lab", "admin", "password", "new", "", Some(&saved)).unwrap();
        assert_eq!(rotated.password.as_str(), "new");
    }

    #[test]
    fn switching_method_drops_the_old_secret_and_key_needs_a_file() {
        let saved = credential_from_form("", "Lab", "root", "password", "pw", "", None).unwrap();
        assert!(credential_from_form(&saved.id, "Lab", "root", "key", "", "", Some(&saved)).is_err());
        let key = credential_from_form(
            &saved.id, "Lab", "root", "key", "", "C:\\keys\\id_ed25519", Some(&saved),
        )
        .unwrap();
        assert!(key.password.is_empty());
        assert_eq!(key.private_key_path, "C:/keys/id_ed25519");
    }

    #[test]
    fn a_pasted_key_is_kept_when_no_file_is_picked() {
        let mut saved = Credential::new_empty();
        saved.name = "Lab".into();
        saved.auth = AuthMethod::Key;
        saved.private_key_inline = Secret::new("-----BEGIN OPENSSH PRIVATE KEY-----");
        let edited =
            credential_from_form(&saved.id, "Lab", "root", "key", "", "", Some(&saved)).unwrap();
        assert_eq!(edited.private_key_inline.as_str(), "-----BEGIN OPENSSH PRIVATE KEY-----");
    }
}
