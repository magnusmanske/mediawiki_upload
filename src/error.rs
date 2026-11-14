//! Errors returned by the uploader.

use crate::filename::FileNameError;
use crate::oauth1::OAuthError;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use thiserror::Error;

/// An error object returned by the MediaWiki Action API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    /// The machine-readable error code, for example `badtoken`.
    pub code: String,

    /// The human-readable explanation, if the API supplied one.
    pub info: Option<String>,
}

impl ApiError {
    pub(crate) fn from_value(value: &Value) -> Self {
        // The Action API uses {code, info}; some extensions (notably OAuth)
        // use a plain string in `error`, with the explanation in `message`.
        let code = value
            .get("code")
            .and_then(Value::as_str)
            .or_else(|| value.as_str())
            .unwrap_or("unknown")
            .to_string();
        let info = value
            .get("info")
            .or_else(|| value.get("message"))
            .map(|info| match info {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            });
        Self { code, info }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let info = self.info.as_deref().map(str::trim).unwrap_or_default();
        match (info.is_empty(), self.code.trim().is_empty()) {
            (false, false) => write!(f, "{info} [{}]", self.code),
            (false, true) => f.write_str(info),
            (true, false) => f.write_str(&self.code),
            (true, true) => f.write_str("unspecified API error"),
        }
    }
}

impl std::error::Error for ApiError {}

/// An upload that the wiki did not accept.
///
/// This includes warnings when they are not ignored. `result` is MediaWiki's
/// own result string (`"Warning"`, `"Continue"`, …), and `warnings` is the
/// map it returned. `file_key` is present when the file was stashed and could
/// still be published.
#[derive(Debug, Clone)]
pub struct UploadFailure {
    /// MediaWiki's upload result, for example `Warning`.
    pub result: String,

    /// The warnings MediaWiki returned, keyed by warning name.
    pub warnings: BTreeMap<String, Value>,

    /// The key of a stashed upload, if the wiki returned one.
    pub file_key: Option<String>,
}

impl UploadFailure {
    fn described_warnings(&self) -> Vec<String> {
        const KNOWN: &[(&str, &str)] = &[
            ("exists", "a different file already uses the name"),
            ("exists-normalized", "a file with an equivalent name exists"),
            (
                "duplicate-archive",
                "an identical file was uploaded and deleted before",
            ),
            ("was-deleted", "this filename was deleted before"),
            ("badfilename", "the wiki would rename the file to"),
            ("emptyfile", "the file is empty"),
            (
                "filetype-unwanted-type",
                "this file type is not wanted on the wiki",
            ),
            (
                "filetype-banned",
                "this file type is not allowed on the wiki",
            ),
            ("large-file", "the file is larger than the wiki allows"),
            (
                "was-deleted-archive",
                "an earlier version of this file was deleted",
            ),
        ];

        let mut parts = Vec::new();
        if let Some(value) = self.warnings.get("duplicate") {
            let first = match value {
                Value::Array(values) => values.first(),
                other => Some(other),
            };
            if let Some(first) = first {
                let name = match first {
                    Value::String(name) => Some(name.clone()),
                    Value::Null | Value::Bool(_) => None,
                    other => Some(other.to_string()),
                };
                if let Some(name) = name.filter(|name| !name.is_empty()) {
                    parts.push(format!("identical file already on the wiki as \"{name}\""));
                }
            }
        }
        for (key, description) in KNOWN {
            let Some(value) = self.warnings.get(*key) else {
                continue;
            };
            match warning_text(value) {
                Some(value) => parts.push(format!("{description} \"{value}\"")),
                None => parts.push((*description).to_string()),
            }
        }
        for (key, value) in &self.warnings {
            if key == "duplicate" || KNOWN.iter().any(|(known, _)| known == key) {
                continue;
            }
            parts.push(format!("{key}: {value}"));
        }
        parts
    }
}

impl fmt::Display for UploadFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.result)?;
        let parts = self.described_warnings();
        if !parts.is_empty() {
            write!(f, ": {}", parts.join("; "))?;
        }
        Ok(())
    }
}

impl std::error::Error for UploadFailure {}

/// A string for a warning value, if it says anything beyond the warning
/// name. Booleans and empty values only confirm that the warning applies.
fn warning_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::String(_) | Value::Null | Value::Bool(_) => None,
        Value::Array(values) if values.is_empty() => None,
        other => Some(other.to_string()),
    }
}

/// Anything that can go wrong while uploading a file.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum UploadError {
    /// The API URL is not a URL.
    #[error("invalid API URL: {0}")]
    InvalidUrl(#[from] url::ParseError),

    /// The API URL does not use HTTP or HTTPS.
    #[error("the API URL must use http or https")]
    InvalidScheme,

    /// The API URL has no host.
    #[error("the API URL has no host")]
    MissingHost,

    /// A file name could not be normalized.
    #[error(transparent)]
    FileName(#[from] FileNameError),

    /// An OAuth 1.0a request could not be signed.
    #[error(transparent)]
    OAuth(#[from] OAuthError),

    /// The HTTP request failed before a response was received.
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    /// The wiki answered with a non-success HTTP status.
    #[error("the wiki API returned HTTP {status}: {body}")]
    HttpStatus {
        /// The status code.
        status: reqwest::StatusCode,
        /// The start of the response body.
        body: String,
    },

    /// The wiki answered with something that is not JSON.
    #[error("the wiki API answer is not JSON: {source}; body: {body}")]
    Json {
        /// The JSON parse error.
        #[source]
        source: serde_json::Error,
        /// The start of the response body.
        body: String,
    },

    /// The wiki returned an API error object.
    #[error(transparent)]
    Api(#[from] ApiError),

    /// The upload was refused or produced warnings that were not ignored.
    #[error(transparent)]
    Upload(#[from] UploadFailure),

    /// The answer has no `upload` member.
    #[error("the wiki API answer has no upload result: {body}")]
    MissingUpload {
        /// The response body.
        body: String,
    },

    /// The answer has no CSRF token.
    #[error("the wiki API answer has no CSRF token: {body}")]
    MissingToken {
        /// The response body.
        body: String,
    },

    /// The wiki stashed the file but did not return a file key to publish it.
    #[error("the wiki stashed the file but returned no file key")]
    MissingFileKey,

    /// A file could not be read for uploading.
    #[error("cannot read {path}: {source}")]
    File {
        /// The file that could not be read.
        path: PathBuf,
        /// The I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The `upload` member has an unexpected shape.
    #[error("malformed upload result: {source}")]
    MalformedUpload {
        /// The JSON parse error.
        #[source]
        source: serde_json::Error,
        /// The malformed value.
        value: Value,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn failure(warnings: Value) -> UploadFailure {
        UploadFailure {
            result: "Warning".to_string(),
            warnings: serde_json::from_value(warnings).unwrap_or_default(),
            file_key: Some("key".to_string()),
        }
    }

    #[test]
    fn describes_duplicate_and_unknown_warnings() {
        let failure = failure(json!({
            "duplicate": ["Other.jpg"],
            "custom-warning": "something"
        }));
        let text = failure.to_string();
        assert!(text.contains("identical file already on the wiki as \"Other.jpg\""));
        assert!(text.contains("custom-warning: \"something\""));
    }

    #[test]
    fn empty_warning_lists_are_not_duplicates() {
        let failure = failure(json!({"duplicate": []}));
        assert_eq!(failure.to_string(), "Warning");
    }

    #[test]
    fn api_error_display_handles_missing_info() {
        let error = ApiError {
            code: "badtoken".to_string(),
            info: Some("Invalid CSRF token".to_string()),
        };
        assert_eq!(error.to_string(), "Invalid CSRF token [badtoken]");
        let error = ApiError {
            code: "badtoken".to_string(),
            info: None,
        };
        assert_eq!(error.to_string(), "badtoken");
    }

    #[test]
    fn api_error_accepts_oauth_style_errors() {
        let error = ApiError::from_value(
            &json!({
                "error": "mwoauth-invalid-authorization",
                "message": "Not authorized"
            })["error"]
                .clone(),
        );
        assert_eq!(error.code, "mwoauth-invalid-authorization");

        let error = ApiError::from_value(&json!("plain error"));
        assert_eq!(error.code, "plain error");
    }
}
