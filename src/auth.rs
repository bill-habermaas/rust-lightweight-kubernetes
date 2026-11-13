//! Token based authentication: HS256 JWTs carrying subject, roles and a token id (jti)
//! that can be revoked. Passwords are stored as Argon2id hashes.

use crate::error::{ApiError, Result};
use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use chrono::Utc;
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenKind {
    /// Interactive login. Roles are re-read from the user record on every request.
    Session,
    /// Long-lived API token (CI, scripts). Roles are fixed in the token.
    Api,
    /// Token issued to a registered operator.
    Operator,
}

impl TokenKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            TokenKind::Session => "session",
            TokenKind::Api => "api",
            TokenKind::Operator => "operator",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub roles: Vec<String>,
    pub kind: TokenKind,
    pub iat: i64,
    pub exp: i64,
    pub jti: String,
    pub iss: String,
}

pub struct TokenManager {
    encoding: EncodingKey,
    decoding: DecodingKey,
    issuer: String,
    default_ttl: i64,
}

impl TokenManager {
    pub fn new(secret: &[u8], issuer: &str, default_ttl_secs: i64) -> Self {
        Self {
            encoding: EncodingKey::from_secret(secret),
            decoding: DecodingKey::from_secret(secret),
            issuer: issuer.into(),
            default_ttl: default_ttl_secs,
        }
    }

    pub fn issue(
        &self,
        subject: &str,
        roles: Vec<String>,
        kind: TokenKind,
        ttl_secs: Option<i64>,
    ) -> Result<(String, Claims)> {
        let now = Utc::now().timestamp();
        let ttl = ttl_secs.filter(|t| *t > 0).unwrap_or(self.default_ttl);
        let claims = Claims {
            sub: subject.into(),
            roles,
            kind,
            iat: now,
            exp: now + ttl,
            jti: uuid::Uuid::new_v4().to_string(),
            iss: self.issuer.clone(),
        };
        let token = encode(&Header::new(Algorithm::HS256), &claims, &self.encoding)
            .map_err(|e| ApiError::Internal(format!("token encoding failed: {e}")))?;
        Ok((token, claims))
    }

    /// Verify signature, issuer and expiry.
    pub fn verify(&self, token: &str) -> Result<Claims> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_issuer(&[&self.issuer]);
        validation.set_required_spec_claims(&["exp", "iss", "sub"]);
        validation.leeway = 5;
        decode::<Claims>(token, &self.decoding, &validation)
            .map(|d| d.claims)
            .map_err(|e| ApiError::Unauthenticated(format!("invalid token: {e}")))
    }
}

pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| ApiError::Internal(format!("password hashing failed: {e}")))
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|parsed| Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
        .unwrap_or(false)
}

/// Extract the bearer token from gRPC metadata (`authorization: Bearer <token>`).
pub fn bearer_token(md: &tonic::metadata::MetadataMap) -> Result<String> {
    let raw = md
        .get("authorization")
        .ok_or_else(|| ApiError::Unauthenticated("missing authorization metadata".into()))?
        .to_str()
        .map_err(|_| ApiError::Unauthenticated("authorization metadata is not valid ASCII".into()))?;
    let (scheme, token) = raw.split_once(' ').unwrap_or(("", ""));
    let token = token.trim();
    if !scheme.eq_ignore_ascii_case("bearer") || token.is_empty() {
        return Err(ApiError::Unauthenticated("expected 'Bearer <token>'".into()));
    }
    Ok(token.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mgr() -> TokenManager {
        TokenManager::new(b"unit-test-secret", "lkube", 3600)
    }

    #[test]
    fn issue_and_verify_roundtrip() {
        let m = mgr();
        let (token, claims) = m.issue("alice", vec!["editor".into()], TokenKind::Session, None).unwrap();
        let verified = m.verify(&token).unwrap();
        assert_eq!(verified, claims);
        assert_eq!(verified.sub, "alice");
        assert_eq!(verified.exp - verified.iat, 3600);
    }

    #[test]
    fn custom_ttl_is_respected() {
        let (_, claims) = mgr().issue("bot", vec![], TokenKind::Api, Some(60)).unwrap();
        assert_eq!(claims.exp - claims.iat, 60);
    }

    #[test]
    fn rejects_wrong_signature() {
        let (token, _) = mgr().issue("alice", vec![], TokenKind::Session, None).unwrap();
        let other = TokenManager::new(b"different-secret", "lkube", 3600);
        assert!(matches!(other.verify(&token), Err(ApiError::Unauthenticated(_))));
    }

    #[test]
    fn rejects_wrong_issuer() {
        let (token, _) = mgr().issue("alice", vec![], TokenKind::Session, None).unwrap();
        let other = TokenManager::new(b"unit-test-secret", "someone-else", 3600);
        assert!(other.verify(&token).is_err());
    }

    #[test]
    fn rejects_expired_token() {
        let now = Utc::now().timestamp();
        let claims = Claims {
            sub: "alice".into(),
            roles: vec![],
            kind: TokenKind::Session,
            iat: now - 7200,
            exp: now - 3600,
            jti: "x".into(),
            iss: "lkube".into(),
        };
        let token = encode(&Header::new(Algorithm::HS256), &claims, &EncodingKey::from_secret(b"unit-test-secret")).unwrap();
        assert!(mgr().verify(&token).is_err());
    }

    #[test]
    fn rejects_garbage() {
        assert!(mgr().verify("not.a.jwt").is_err());
        assert!(mgr().verify("").is_err());
    }

    #[test]
    fn password_hashing() {
        let h = hash_password("s3cret-pass").unwrap();
        assert!(h.starts_with("$argon2"));
        assert!(verify_password("s3cret-pass", &h));
        assert!(!verify_password("wrong", &h));
        assert!(!verify_password("s3cret-pass", "not-a-hash"));
    }

    #[test]
    fn bearer_extraction() {
        let mut md = tonic::metadata::MetadataMap::new();
        assert!(bearer_token(&md).is_err());
        md.insert("authorization", "Bearer abc.def".parse().unwrap());
        assert_eq!(bearer_token(&md).unwrap(), "abc.def");
        md.insert("authorization", "bearer xyz".parse().unwrap());
        assert_eq!(bearer_token(&md).unwrap(), "xyz");
        md.insert("authorization", "Basic xyz".parse().unwrap());
        assert!(bearer_token(&md).is_err());
        md.insert("authorization", "Bearer ".parse().unwrap());
        assert!(bearer_token(&md).is_err());
    }
}
