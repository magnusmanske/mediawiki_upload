//! OAuth 1.0a signing for MediaWiki's Action API.
//!
//! The upload tools act on behalf of a user with an OAuth consumer and an
//! access token. Only request signing is implemented here; obtaining the
//! tokens (the login handshake) is the caller's business.

use base64::Engine;
use hmac::{Hmac, Mac};
use sha1::Sha1;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use url::Url;

type HmacSha1 = Hmac<Sha1>;

/// A nonce only has to be unique, not unpredictable; a per-process counter
/// plus the time and process id is enough and needs no random generator.
static NONCE: AtomicU64 = AtomicU64::new(0);

/// Failure modes when signing an OAuth 1.0a request.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OAuthError {
    /// The URL passed to the signer could not be parsed.
    #[error("invalid URL for OAuth signing: {0}")]
    InvalidUrl(#[from] url::ParseError),

    /// The URL has no host, so it cannot appear in an OAuth base string.
    #[error("the URL for OAuth signing has no host")]
    MissingHost,

    /// The HMAC key could not be set up.
    #[error("could not sign the OAuth request: {0}")]
    Signing(String),
}

/// A MediaWiki OAuth 1.0a consumer together with an access token.
///
/// The secrets are deliberately not printed by [`Debug`], and the access
/// token is only ever placed in a signed `Authorization` header.
#[derive(Clone)]
pub struct OAuth1 {
    consumer_key: String,
    consumer_secret: String,
    access_token: String,
    access_token_secret: String,
}

impl OAuth1 {
    /// Creates an OAuth 1.0a signer from a registered consumer and an access
    /// token obtained for it.
    pub fn new(
        consumer_key: impl Into<String>,
        consumer_secret: impl Into<String>,
        access_token: impl Into<String>,
        access_token_secret: impl Into<String>,
    ) -> Self {
        Self {
            consumer_key: consumer_key.into(),
            consumer_secret: consumer_secret.into(),
            access_token: access_token.into(),
            access_token_secret: access_token_secret.into(),
        }
    }

    /// The `Authorization: OAuth …` header value for a request.
    ///
    /// `params` are the request's query or form parameters. Pass an empty
    /// slice for a multipart request: OAuth 1.0a excludes multipart bodies
    /// from the signature, and MediaWiki follows that.
    pub fn authorization_header(
        &self,
        method: &str,
        url: &str,
        params: &[(String, String)],
    ) -> Result<String, OAuthError> {
        let mut oauth = self.oauth_params();
        let mut signed = params.to_vec();
        signed.extend(oauth.iter().cloned());
        let signature = sign(
            method,
            url,
            &signed,
            &self.consumer_secret,
            &self.access_token_secret,
        )?;
        oauth.push(("oauth_signature".to_string(), signature));

        let fields: Vec<String> = oauth
            .iter()
            .map(|(key, value)| format!("{}=\"{}\"", encode(key), encode(value)))
            .collect();
        Ok(format!("OAuth {}", fields.join(", ")))
    }

    fn oauth_params(&self) -> Vec<(String, String)> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default();
        let counter = NONCE.fetch_add(1, Ordering::Relaxed);
        let nonce = format!("{timestamp:x}{counter:x}{:x}", std::process::id());

        vec![
            ("oauth_consumer_key".to_string(), self.consumer_key.clone()),
            ("oauth_token".to_string(), self.access_token.clone()),
            ("oauth_version".to_string(), "1.0".to_string()),
            ("oauth_nonce".to_string(), nonce),
            ("oauth_timestamp".to_string(), timestamp.to_string()),
            (
                "oauth_signature_method".to_string(),
                "HMAC-SHA1".to_string(),
            ),
        ]
    }
}

impl fmt::Debug for OAuth1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuth1")
            .field("consumer_key", &self.consumer_key)
            .field("consumer_secret", &"<redacted>")
            .field("access_token", &self.access_token)
            .field("access_token_secret", &"<redacted>")
            .finish()
    }
}

/// HMAC-SHA1 signature of a request, as PHP's `MW_OAuth::sign_request` made it.
fn sign(
    method: &str,
    url: &str,
    params: &[(String, String)],
    consumer_secret: &str,
    token_secret: &str,
) -> Result<String, OAuthError> {
    let parsed = Url::parse(url)?;
    let host = parsed.host_str().ok_or(OAuthError::MissingHost)?;
    let scheme = parsed.scheme();

    let mut base = format!("{scheme}://{host}");
    if let Some(port) = parsed.port()
        && !matches!((scheme, port), ("http", 80) | ("https", 443))
    {
        base.push_str(&format!(":{port}"));
    }
    base.push_str(parsed.path());

    let mut pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .chain(params.iter().cloned())
        .filter(|(key, _)| key != "oauth_signature")
        .map(|(key, value)| (encode(&key), encode(&value)))
        .collect();
    pairs.sort();

    let joined = pairs
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&");
    let base_string = format!(
        "{}&{}&{}",
        encode(&method.to_uppercase()),
        encode(&base),
        encode(&joined)
    );
    let key = format!("{}&{}", encode(consumer_secret), encode(token_secret));

    let mut mac = HmacSha1::new_from_slice(key.as_bytes())
        .map_err(|error| OAuthError::Signing(error.to_string()))?;
    mac.update(base_string.as_bytes());
    Ok(base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes()))
}

/// RFC 3986 percent-encoding, matching PHP's `rawurlencode`.
fn encode(value: &str) -> String {
    urlencoding::encode(value).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(params: &[(&str, &str)]) -> Vec<(String, String)> {
        params
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    // The expected signatures were computed with the PHP original.
    #[test]
    fn signatures_match_php() {
        let params = pairs(&[
            ("action", "query"),
            ("meta", "userinfo"),
            ("uiprop", "blockinfo|groups|rights"),
            ("oauth_consumer_key", "ck"),
            ("oauth_token", "tk"),
            ("oauth_nonce", "n0nce"),
            ("oauth_timestamp", "1700000000"),
            ("oauth_version", "1.0"),
            ("oauth_signature_method", "HMAC-SHA1"),
            ("text", "Ä ~*!"),
        ]);
        let signature = sign(
            "POST",
            "https://www.mediawiki.org/w/api.php",
            &params,
            "consumer&secret",
            "token secret",
        )
        .expect("signing");
        assert_eq!(signature, "LGExTIHUCc6kOdGfRPmri7bcuMQ=");
    }

    #[test]
    fn query_parameters_are_signed() {
        let url = "https://www.mediawiki.org/wiki/Special:OAuth/initiate?format=json\
            &oauth_callback=https%3A%2F%2Fx.org%2Fa.php&oauth_consumer_key=ck&oauth_version=1.0&oauth_nonce=abc\
            &oauth_timestamp=1700000000&oauth_signature_method=HMAC-SHA1";
        assert_eq!(
            sign("GET", url, &[], "consumer&secret", "token secret").expect("signing"),
            "5RmCvDDm38ICZHopQnMZofOAuIA="
        );
    }

    #[test]
    fn signature_parameter_is_not_signed_again() {
        let url = "https://example.org/w/api.php";
        let mut params = pairs(&[("action", "query")]);
        params.push(("oauth_signature".to_string(), "ignored".to_string()));
        let without = sign("POST", url, &params[..1], "consumer", "token").expect("signing");
        let with = sign("POST", url, &params, "consumer", "token").expect("signing");
        assert_eq!(without, with);
    }

    #[test]
    fn secrets_are_redacted_in_debug() {
        let oauth = OAuth1::new("ck", "consumer-secret", "tk", "token-secret");
        let debug = format!("{oauth:?}");
        assert!(!debug.contains("consumer-secret"));
        assert!(!debug.contains("token-secret"));
        assert!(debug.contains("ck"));
    }

    #[test]
    fn authorization_header_contains_signature() {
        let oauth = OAuth1::new("ck", "consumer-secret", "tk", "token-secret");
        let header = oauth
            .authorization_header("POST", "https://example.org/w/api.php", &[])
            .expect("header");
        assert!(header.starts_with("OAuth "));
        assert!(header.contains("oauth_consumer_key=\"ck\""));
        assert!(header.contains("oauth_token=\"tk\""));
        assert!(header.contains("oauth_signature=\""));
    }
}
