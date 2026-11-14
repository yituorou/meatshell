use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{AuthMethod, Secret, Session, SessionKind};

/// A saved SSH login shared by several sessions (#526), like WindTerm's
/// OneKey: many servers with the same username and password point at one
/// credential instead of each keeping its own copy. Changing the password here
/// changes it for every session that uses it.
///
/// Secrets are encrypted at rest exactly like session secrets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credential {
    pub id: String,
    /// Name shown in the session editor's picker and in Settings.
    pub name: String,
    #[serde(default)]
    pub user: String,
    pub auth: AuthMethod,
    /// Password (password / keyboard-interactive), or the key passphrase (key).
    #[serde(default)]
    pub password: Secret,
    #[serde(default)]
    pub private_key_path: String,
    #[serde(default)]
    pub private_key_inline: Secret,
}

impl Credential {
    pub fn new_empty() -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name: String::new(),
            user: String::new(),
            auth: AuthMethod::Password,
            password: Secret::default(),
            private_key_path: String::new(),
            private_key_inline: Secret::default(),
        }
    }

    /// Copy this login into `session`: username, method and secrets.
    pub fn apply_to(&self, session: &mut Session) {
        session.user = self.user.clone();
        session.auth = self.auth;
        session.password = self.password.clone();
        session.private_key_path = self.private_key_path.clone();
        session.private_key_inline = self.private_key_inline.clone();
    }

    /// One-line summary for lists: `root · password`.
    pub fn summary(&self) -> String {
        let method = match self.auth {
            AuthMethod::Password => "password",
            AuthMethod::KeyboardInteractive => "keyboard-interactive",
            AuthMethod::Key if !self.private_key_inline.is_empty() => "pasted key",
            AuthMethod::Key => "key file",
        };
        if self.user.is_empty() {
            method.to_string()
        } else {
            format!("{} · {method}", self.user)
        }
    }
}

/// Fill a linked session's login from its credential. Only SSH sessions can be
/// linked; a link on another kind, or to a credential that no longer exists,
/// is dropped and the session keeps the login it already holds.
pub(crate) fn apply_linked_credential(credentials: &[Credential], session: &mut Session) {
    if session.credential_id.is_empty() {
        return;
    }
    if session.kind != SessionKind::Ssh {
        session.credential_id.clear();
        return;
    }
    match credentials.iter().find(|c| c.id == session.credential_id) {
        Some(credential) => credential.apply_to(session),
        None => session.credential_id.clear(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credential() -> Credential {
        Credential {
            name: "ops".into(),
            user: "deploy".into(),
            auth: AuthMethod::Key,
            password: Secret::new("passphrase"),
            private_key_path: "/home/me/.ssh/id_ed25519".into(),
            ..Credential::new_empty()
        }
    }

    #[test]
    fn linked_ssh_session_takes_the_credential_login() {
        let cred = credential();
        let mut session = Session::new_empty();
        session.user = "root".into();
        session.password = Secret::new("old");
        session.credential_id = cred.id.clone();
        apply_linked_credential(std::slice::from_ref(&cred), &mut session);
        assert_eq!(session.user, "deploy");
        assert_eq!(session.auth, AuthMethod::Key);
        assert_eq!(session.password.as_str(), "passphrase");
        assert_eq!(session.private_key_path, "/home/me/.ssh/id_ed25519");
        assert_eq!(session.credential_id, cred.id);
    }

    #[test]
    fn dangling_or_non_ssh_links_are_dropped_and_login_kept() {
        let cred = credential();
        let mut dangling = Session::new_empty();
        dangling.user = "root".into();
        dangling.credential_id = "missing".into();
        apply_linked_credential(std::slice::from_ref(&cred), &mut dangling);
        assert!(dangling.credential_id.is_empty());
        assert_eq!(dangling.user, "root");

        let mut telnet = Session::new_empty();
        telnet.kind = SessionKind::Telnet;
        telnet.user = "admin".into();
        telnet.credential_id = cred.id.clone();
        apply_linked_credential(std::slice::from_ref(&cred), &mut telnet);
        assert!(telnet.credential_id.is_empty());
        assert_eq!(telnet.user, "admin");
    }

    #[test]
    fn debug_output_never_shows_secrets() {
        let text = format!("{:?}", credential());
        assert!(!text.contains("passphrase"));
    }
}
