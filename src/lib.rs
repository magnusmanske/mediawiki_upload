//! Upload files to a MediaWiki wiki through its Action API.
//!
//! This is the upload primitive shared by the PHP tools `url2commons`,
//! `flickr2commons`, `commonshelper` and `geograph2commons`: a signed
//! `action=upload` multipart request, with the CSRF token fetch and the
//! warning/stash handling those tools need.
//!
//! Getting the OAuth access token (the login handshake) is left to the
//! caller. Fetching a source URL is also left to the caller: URL allow-listing
//! and SSRF protection belong to the tool that accepts the URL. Download the
//! file, then call [`Uploader::upload_file`] or [`Uploader::upload_bytes`].
//!
//! ```no_run
//! use mediawiki_upload::{Credentials, FileName, UploadRequest, Uploader};
//!
//! # async fn run() -> Result<(), mediawiki_upload::UploadError> {
//! let uploader = Uploader::commons(
//!     "my-tool/1.0 (https://example.org; me@example.org)",
//!     Credentials::oauth1("consumer key", "consumer secret", "access token", "access secret"),
//! )?;
//!
//! let request = UploadRequest::new(FileName::new("Example photo.jpg")?)
//!     .text("== {{int:filedesc}} ==\n{{Information|Description=…}}\n")
//!     .comment("Transferred from https://example.org/photo")
//!     .ignore_warnings(true)
//!     .block_duplicates();
//!
//! let result = uploader.upload_file("photo.jpg", request).await?;
//! println!("uploaded as {}", result.filename);
//! # Ok(())
//! # }
//! ```
//!
//! The tool's own upload semantics are preserved:
//!
//! * file names are normalized as `ucfirst(str_replace(' ', '_', $name))`;
//! * `ignorewarnings` is only sent when the caller asks for it;
//! * warnings can be inspected first, and cosmetic warnings can then be
//!   overridden while byte-identical duplicates still stop the upload
//!   ([`UploadRequest::block_duplicates`]);
//! * large files can be uploaded in chunks ([`UploadRequest::chunked`]);
//! * OAuth 1.0a signatures exclude the multipart body, as MediaWiki expects.
//!
//! With the `wikimedia` feature, the `wikimedia` module adds Commons
//! pre-upload checks (file name, size, SHA-1 duplicates, deletion log) through
//! the API; the `toolforge-replicas` feature adds the same checks against the
//! Toolforge wiki replicas.
//!
//! There is deliberately no automatic retry: an upload is not always
//! idempotent, and a retry policy belongs to the tool that knows whether the
//! previous attempt may have succeeded.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![cfg_attr(
    not(test),
    warn(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]
#![deny(missing_docs)]

mod credentials;
mod error;
mod filename;
mod oauth1;
mod uploader;

#[cfg(feature = "wikimedia")]
pub mod wikimedia;

pub use credentials::Credentials;
pub use error::{ApiError, UploadError, UploadFailure};
pub use filename::{FileName, FileNameError};
pub use oauth1::{OAuth1, OAuthError};
pub use uploader::{COMMONS_API, DEFAULT_CHUNK_SIZE, UploadRequest, UploadSuccess, Uploader};
