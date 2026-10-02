use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

/// A secret string (e.g. a session password) whose heap buffer is zeroed when
/// it is dropped, so plaintext credentials don't survive in freed memory and
/// turn up in core dumps, a debugger, or `/proc/<pid>/mem`.  `Clone` makes an
/// independent copy that is likewise zeroed on its own drop, and `Debug` is
/// redacted so a password can never be logged by accident.
#[derive(Clone, Default)]
pub struct Secret {
    value: String,
    // Nonserialized provenance: only raw local-profile deserialization can mark
    // a value as ciphertext. Successfully decoded or newly entered values are
    // plaintext even if their literal content begins with `enc:v1:`.
    local_ciphertext: bool,
}

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Self {
            value: s.into(),
            local_ciphertext: false,
        }
    }
    pub fn as_str(&self) -> &str {
        &self.value
    }
    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }
    pub(crate) fn is_local_ciphertext(&self) -> bool {
        self.local_ciphertext
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.value.zeroize();
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never reveal the contents in logs / debug output.
        f.write_str(if self.value.is_empty() {
            "Secret(\"\")"
        } else {
            "Secret(***)"
        })
    }
}

impl Serialize for Secret {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.value)
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = String::deserialize(d)?;
        let local_ciphertext = value.starts_with("enc:v1:");
        Ok(Self {
            value,
            local_ciphertext,
        })
    }
}
