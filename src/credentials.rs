//! How requests to the wiki API are authenticated.

use crate::oauth1::OAuth1;
use std::fmt;

/// Authentication for the wiki API.
///
/// Uploads need a logged-in user. OAuth 1.0a is what the existing Toolforge
/// upload tools use; OAuth 2 bearer tokens are accepted by MediaWiki as well.
#[derive(Clone)]
#[non_exhaustive]
pub enum Credentials {
    /// OAuth 1.0a consumer and access token.
    OAuth1(OAuth1),

    /// An OAuth 2 access token, sent as `Authorization: Bearer …`.
    Bearer(String),

    /// No `Authorization` header. Use this with a [`reqwest::Client`] that
    /// carries session cookies, for example after a bot-password login.
    None,
}

impl Credentials {
    /// OAuth 1.0a credentials.
    pub fn oauth1(
        consumer_key: impl Into<String>,
        consumer_secret: impl Into<String>,
        access_token: impl Into<String>,
        access_token_secret: impl Into<String>,
    ) -> Self {
        Self::OAuth1(OAuth1::new(
            consumer_key,
            consumer_secret,
            access_token,
            access_token_secret,
        ))
    }

    /// An OAuth 2 bearer token.
    pub fn bearer(token: impl Into<String>) -> Self {
        Self::Bearer(token.into())
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OAuth1(oauth) => f.debug_tuple("OAuth1").field(oauth).finish(),
            Self::Bearer(_) => f.debug_tuple("Bearer").field(&"<redacted>").finish(),
            Self::None => f.write_str("None"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_secret_is_redacted_in_debug() {
        let credentials = Credentials::bearer("secret-token");
        let debug = format!("{credentials:?}");
        assert!(!debug.contains("secret-token"));
        assert!(debug.contains("redacted"));
    }

    #[test]
    fn oauth_secrets_are_redacted_in_debug() {
        let credentials = Credentials::oauth1("ck", "consumer-secret", "tk", "token-secret");
        let debug = format!("{credentials:?}");
        assert!(!debug.contains("consumer-secret"));
        assert!(!debug.contains("token-secret"));
    }
}
