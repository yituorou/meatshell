//! OAuth resource-server verification only. Login/PKCE/token issuance belongs to
//! an established external authorization server, never to MeatShell.
use anyhow::{ensure, Context, Result};
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use std::{
    collections::HashMap,
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OAuthConfig {
    pub issuer: String,
    pub resource: String,
    pub jwks_file: String,
    pub required_scope: String,
    pub allowed_subjects: Vec<String>,
}

pub(super) struct OAuth {
    pub config: OAuthConfig,
    keys: HashMap<String, DecodingKey>,
    validation: Validation,
}

#[derive(Clone)]
pub(super) struct Principal {
    pub subject: String,
    pub expires_at: u64,
}

#[derive(Debug)]
pub(super) enum AuthError {
    InvalidToken,
    InsufficientPermission,
}

#[derive(Deserialize)]
struct Claims {
    sub: String,
    exp: u64,
    #[serde(default)]
    scope: String,
}

pub(super) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(super) fn remaining(expires_at: u64) -> std::time::Duration {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    std::time::Duration::from_secs(expires_at)
        .saturating_sub(now)
        .min(std::time::Duration::from_secs(300))
}

/// URLs contain public metadata only. Reject credentials/query/fragment, and
/// require HTTPS even when the actual listener is a loopback reverse-proxy hop.
pub(super) fn https_url(value: &str) -> Result<url::Url> {
    let url = url::Url::parse(value).context("invalid OAuth URL")?;
    ensure!(
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "OAuth URLs require HTTPS and cannot contain credentials, query or fragment"
    );
    Ok(url)
}

impl OAuth {
    pub fn load(config: OAuthConfig, base: &Path) -> Result<Self> {
        https_url(&config.issuer)?;
        let resource = https_url(&config.resource)?;
        ensure!(
            resource.path() == "/mcp",
            "OAuth resource must be the canonical HTTPS /mcp URL"
        );
        ensure!(
            !config.required_scope.is_empty()
                && config
                    .required_scope
                    .bytes()
                    .all(|b| b == 0x21 || (0x23..=0x5b).contains(&b) || (0x5d..=0x7e).contains(&b)),
            "required_scope must be one OAuth scope token"
        );
        ensure!(
            !config.allowed_subjects.is_empty()
                && config.allowed_subjects.len() <= 64
                && config.allowed_subjects.iter().all(|s| !s.trim().is_empty()),
            "allowed_subjects must explicitly allow 1..64 OAuth subjects for this profile"
        );
        let path = base.join(&config.jwks_file);
        ensure!(
            fs::metadata(&path)
                .context("read public JWKS metadata")?
                .len()
                <= 256 * 1024,
            "public JWKS exceeds 256 KiB"
        );
        let document: serde_json::Value =
            serde_json::from_slice(&fs::read(path).context("read public JWKS")?)
                .context("parse public JWKS")?;
        let jwks = document
            .get("keys")
            .and_then(|v| v.as_array())
            .context("JWKS keys must be an array")?;
        ensure!(
            !jwks.is_empty() && jwks.len() <= 32,
            "JWKS must contain 1..32 public keys"
        );
        let mut keys = HashMap::new();
        for key in jwks {
            // Reject private or symmetric keys even if serde's public JWK type
            // would silently ignore those extra fields.
            ensure!(
                ["d", "p", "q", "dp", "dq", "qi", "oth", "k"]
                    .iter()
                    .all(|field| key.get(field).is_none()),
                "JWKS must contain public keys only"
            );
            if key["kty"] != "RSA" || key.get("alg").is_some_and(|v| v != "RS256") {
                continue;
            }
            ensure!(
                key.get("use").is_none_or(|v| v == "sig"),
                "JWKS key must be for signatures"
            );
            ensure!(
                key.get("key_ops").is_none_or(|v| v
                    .as_array()
                    .is_some_and(|ops| ops.len() == 1 && ops[0] == "verify")),
                "JWKS key_ops must be verify only"
            );
            let kid = key["kid"]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 256)
                .context("RS256 public key needs kid")?;
            ensure!(!keys.contains_key(kid), "duplicate JWKS kid");
            let jwk = serde_json::from_value(key.clone()).context("invalid RSA public JWK")?;
            keys.insert(
                kid.to_owned(),
                DecodingKey::from_jwk(&jwk).context("invalid RSA public key")?,
            );
        }
        ensure!(
            !keys.is_empty(),
            "JWKS contains no usable RS256 public keys"
        );
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&[&config.issuer]);
        validation.set_audience(&[&config.resource]);
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        validation.validate_nbf = true;
        validation.leeway = 0;
        Ok(Self {
            config,
            keys,
            validation,
        })
    }

    pub fn verify(&self, token: &str) -> std::result::Result<Principal, AuthError> {
        use AuthError::*;
        if token.len() > 16 * 1024 {
            return Err(InvalidToken);
        }
        let header = decode_header(token).map_err(|_| InvalidToken)?;
        if header.alg != Algorithm::RS256 {
            return Err(InvalidToken);
        }
        let key = self
            .keys
            .get(header.kid.as_deref().ok_or(InvalidToken)?)
            .ok_or(InvalidToken)?;
        let claims = decode::<Claims>(token, key, &self.validation)
            .map_err(|_| InvalidToken)?
            .claims;
        if claims.exp <= now() {
            return Err(InvalidToken);
        }
        if !self.config.allowed_subjects.contains(&claims.sub)
            || !claims
                .scope
                .split_ascii_whitespace()
                .any(|scope| scope == self.config.required_scope)
        {
            return Err(InsufficientPermission);
        }
        Ok(Principal {
            subject: claims.sub,
            expires_at: claims.exp,
        })
    }

    pub fn metadata(&self) -> serde_json::Value {
        serde_json::json!({"resource": self.config.resource,
            "authorization_servers": [self.config.issuer],
            "scopes_supported": [self.config.required_scope],
            "bearer_methods_supported": ["header"]})
    }

    pub fn challenge(&self) -> String {
        let mut url = https_url(&self.config.resource).expect("validated resource");
        url.set_path("/.well-known/oauth-protected-resource/mcp");
        format!(
            "Bearer resource_metadata=\"{url}\", scope=\"{}\"",
            self.config.required_scope
        )
    }
}
