//! Wikimedia-specific support, with a focus on Wikimedia Commons.
//!
//! The uploader itself is generic. This module adds the things the Commons
//! transfer tools need before they upload: SHA-1 and size facts for a local
//! file, and checks for an existing file name, identical files (including
//! deleted ones), and the wiki's upload-size limit.
//!
//! The API implementation works anywhere. With the `toolforge-replicas`
//! feature, the same checks can be made against the Toolforge wiki replicas.

use crate::error::ApiError;
use crate::filename::FileName;
use crate::uploader::COMMONS_API;
use reqwest::{Client, Url};
use serde_json::Value;
use sha1::{Digest, Sha1 as Sha1Hasher};
use std::fmt;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;
use thiserror::Error;
use tokio::io::AsyncReadExt;

#[cfg(feature = "toolforge-replicas")]
mod replicas;
#[cfg(feature = "toolforge-replicas")]
pub use replicas::CommonsReplicas;

/// How many duplicate entries the API checks return.
const MAX_RESULTS: &str = "50";

/// Failure modes of the Wikimedia helpers.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum WikimediaError {
    /// A SHA-1 value is not valid hex or base36.
    #[error("invalid SHA-1 hash: {0}")]
    InvalidSha1(String),

    /// The API URL is not a URL.
    #[error("invalid API URL: {0}")]
    InvalidUrl(#[from] url::ParseError),

    /// The API URL does not use HTTP or HTTPS.
    #[error("the API URL must use http or https")]
    InvalidScheme,

    /// The API URL has no host.
    #[error("the API URL has no host")]
    MissingHost,

    /// The HTTP request failed.
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

    /// A local file could not be read.
    #[error("cannot read {path}: {source}")]
    File {
        /// The file that could not be read.
        path: PathBuf,
        /// The I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The API answer does not have the shape this module expects.
    #[error("unexpected API answer: {message}")]
    Unexpected {
        /// What was unexpected.
        message: String,
    },

    /// The Toolforge replica pool failed.
    #[cfg(feature = "toolforge-replicas")]
    #[error("the Toolforge replica pool failed: {0}")]
    Toolforge(#[from] toolforge::Error),

    /// A Toolforge replica query failed.
    #[cfg(feature = "toolforge-replicas")]
    #[error("the Toolforge replica query failed: {0}")]
    Database(#[from] toolforge::pool::mysql_async::Error),
}

/// A SHA-1 digest, held as bytes so that it compares the same whether it came
/// from the API (hex) or from a replica database (base36).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sha1Hash([u8; 20]);

impl Sha1Hash {
    /// Parses the 40-character hexadecimal form MediaWiki's API returns.
    pub fn from_hex(hex: &str) -> Result<Self, WikimediaError> {
        let text = hex.trim();
        if text.len() != 40 {
            return Err(WikimediaError::InvalidSha1(hex.to_string()));
        }
        let bytes = text.as_bytes();
        let mut digest = [0u8; 20];
        for (index, target) in digest.iter_mut().enumerate() {
            let high = hex_digit(bytes[index * 2]);
            let low = hex_digit(bytes[index * 2 + 1]);
            match (high, low) {
                (Some(high), Some(low)) => *target = (high << 4) | low,
                _ => return Err(WikimediaError::InvalidSha1(hex.to_string())),
            }
        }
        Ok(Self(digest))
    }

    /// Parses the base36 form MediaWiki stores in its `img_sha1` and
    /// `fa_sha1` database columns.
    pub fn from_base36(base36: &str) -> Result<Self, WikimediaError> {
        let text = base36.trim();
        if text.is_empty() || text.len() > 31 {
            return Err(WikimediaError::InvalidSha1(base36.to_string()));
        }
        let mut digest = [0u8; 20];
        for character in text.bytes() {
            let digit = match character {
                b'0'..=b'9' => character - b'0',
                b'a'..=b'z' => character - b'a' + 10,
                b'A'..=b'Z' => character - b'A' + 10,
                _ => return Err(WikimediaError::InvalidSha1(base36.to_string())),
            };
            let mut carry = u32::from(digit);
            for byte in digest.iter_mut().rev() {
                let value = u32::from(*byte) * 36 + carry;
                *byte = value as u8;
                carry = value >> 8;
            }
            if carry != 0 {
                return Err(WikimediaError::InvalidSha1(base36.to_string()));
            }
        }
        Ok(Self(digest))
    }

    /// Takes a 20-byte SHA-1 digest.
    pub fn from_digest(digest: [u8; 20]) -> Self {
        Self(digest)
    }

    /// The 40-character lowercase hexadecimal form.
    pub fn to_hex(self) -> String {
        let mut text = String::with_capacity(40);
        for byte in self.0 {
            let _ = write!(text, "{byte:02x}");
        }
        text
    }

    /// The lowercase base36 form MediaWiki uses in its database columns,
    /// zero padded to 31 digits as `FSFile::getSha1Base36()` does.
    pub fn to_base36(self) -> String {
        const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
        let mut bytes = self.0;
        let mut digits = Vec::new();
        loop {
            let mut remainder = 0u32;
            let mut all_zero = true;
            for byte in &mut bytes {
                let value = (remainder << 8) | u32::from(*byte);
                *byte = (value / 36) as u8;
                remainder = value % 36;
                if *byte != 0 {
                    all_zero = false;
                }
            }
            digits.push(DIGITS[remainder as usize] as char);
            if all_zero {
                break;
            }
        }
        let mut text: String = digits.iter().rev().collect();
        while text.len() < 31 {
            text.insert(0, '0');
        }
        text
    }
}

impl fmt::Display for Sha1Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Sha1Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Sha1Hash").field(&self.to_hex()).finish()
    }
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// The facts about a local file that a Commons check needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFacts {
    /// The file size in bytes.
    pub size: u64,
    /// The SHA-1 digest of the file contents.
    pub sha1: Sha1Hash,
}

/// Computes size and SHA-1 for a file, reading it in bounded chunks.
pub async fn file_facts(path: impl AsRef<Path>) -> Result<FileFacts, WikimediaError> {
    let path = path.as_ref();
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|source| WikimediaError::File {
            path: path.to_path_buf(),
            source,
        })?;
    let mut hasher = Sha1Hasher::new();
    let mut size = 0u64;
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|source| WikimediaError::File {
                path: path.to_path_buf(),
                source,
            })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size += read as u64;
    }
    Ok(FileFacts {
        size,
        sha1: Sha1Hash::from_digest(finish_sha1(hasher)),
    })
}

/// Computes size and SHA-1 for an in-memory file.
pub fn bytes_facts(bytes: &[u8]) -> FileFacts {
    FileFacts {
        size: bytes.len() as u64,
        sha1: Sha1Hash::from_digest(digest_array(Sha1Hasher::digest(bytes))),
    }
}

fn finish_sha1(hasher: Sha1Hasher) -> [u8; 20] {
    digest_array(hasher.finalize())
}

fn digest_array(digest: impl AsRef<[u8]>) -> [u8; 20] {
    let mut bytes = [0u8; 20];
    for (target, source) in bytes.iter_mut().zip(digest.as_ref()) {
        *target = *source;
    }
    bytes
}

/// A file that already exists, or existed, on the wiki.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    /// The file name, without a `File:` prefix.
    pub name: String,
    /// The file size in bytes.
    pub size: u64,
    /// The SHA-1 of the file contents.
    pub sha1: Sha1Hash,
    /// The MIME type, when the API supplied one.
    pub mime: Option<String>,
    /// The file URL, when the API supplied one.
    pub url: Option<String>,
}

/// What Commons has to say about a candidate upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommonsCheck {
    /// Whether a file page with the target name exists (even without a file).
    pub page_exists: bool,
    /// The file currently stored under the target name, if any.
    pub existing_file: Option<FileInfo>,
    /// Current files with the same SHA-1, under any name.
    pub duplicates: Vec<FileInfo>,
    /// Deleted files with the same SHA-1.
    pub deleted_duplicates: Vec<FileInfo>,
    /// Whether the target name appears in the deletion log.
    pub was_deleted: bool,
    /// The wiki's maximum upload size, when the check could read it.
    pub max_upload_size: Option<u64>,
    /// The candidate's size in bytes.
    pub size: u64,
    /// The candidate's SHA-1.
    pub sha1: Sha1Hash,
}

impl CommonsCheck {
    /// No page with the target name and no identical current or deleted file.
    ///
    /// A name in the deletion log does not make this false; see
    /// [`Self::needs_review`].
    pub fn is_free(&self) -> bool {
        !self.page_exists
            && self.existing_file.is_none()
            && self.duplicates.is_empty()
            && self.deleted_duplicates.is_empty()
    }

    /// The target name holds exactly the candidate's contents.
    pub fn same_as_existing(&self) -> bool {
        self.existing_file
            .as_ref()
            .is_some_and(|file| file.sha1 == self.sha1 && file.size == self.size)
    }

    /// The candidate is not larger than the wiki's upload limit.
    pub fn size_allowed(&self) -> bool {
        self.max_upload_size
            .is_none_or(|maximum| self.size <= maximum)
    }

    /// There is a reason to look at the warnings before uploading.
    pub fn needs_review(&self) -> bool {
        self.was_deleted || !self.deleted_duplicates.is_empty()
    }
}

/// The upload limits Commons reports in its site info.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CommonsSiteInfo {
    /// The largest file the wiki accepts, in bytes.
    pub max_upload_size: Option<u64>,
    /// The smallest non-final chunk the wiki accepts, in bytes.
    pub min_upload_chunk_size: Option<u64>,
}

/// A file page and, if present, its file information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePage {
    /// Whether the page exists, even if it has no current file.
    pub exists: bool,
    /// The current file, if the page has one.
    pub file: Option<FileInfo>,
}

/// Commons checks through the MediaWiki Action API.
#[derive(Clone)]
pub struct CommonsClient {
    client: Client,
    api_url: Url,
}

impl fmt::Debug for CommonsClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommonsClient")
            .field("api_url", &self.api_url)
            .finish_non_exhaustive()
    }
}

impl CommonsClient {
    /// Creates a client for Wikimedia Commons.
    pub fn new(user_agent: impl AsRef<str>) -> Result<Self, WikimediaError> {
        let client = Client::builder()
            .user_agent(user_agent.as_ref())
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()?;
        Self::with_client(client)
    }

    /// Creates a client around an existing HTTP client, for Wikimedia Commons.
    pub fn with_client(client: Client) -> Result<Self, WikimediaError> {
        Self::with_client_and_url(client, COMMONS_API)
    }

    /// Creates a client around an existing HTTP client and a specific API URL.
    ///
    /// This is mainly useful for tests and non-Wikimedia installations that
    /// still want the Commons-shaped queries.
    pub fn with_client_and_url(
        client: Client,
        api_url: impl AsRef<str>,
    ) -> Result<Self, WikimediaError> {
        let api_url = Url::parse(api_url.as_ref())?;
        if !matches!(api_url.scheme(), "http" | "https") {
            return Err(WikimediaError::InvalidScheme);
        }
        if api_url.host_str().is_none() {
            return Err(WikimediaError::MissingHost);
        }
        Ok(Self { client, api_url })
    }

    /// The Action API URL this client talks to.
    pub fn api_url(&self) -> &Url {
        &self.api_url
    }

    /// The page for `filename`, with its current file if there is one.
    pub async fn file_page(&self, filename: &FileName) -> Result<FilePage, WikimediaError> {
        let title = format!("File:{}", filename);
        let answer = self
            .get(&[
                ("action", "query"),
                ("prop", "info|imageinfo"),
                ("titles", &title),
                ("iiprop", "url|size|sha1|mime"),
                ("iilimit", "1"),
            ])
            .await?;
        let Some(page) = answer.pointer("/query/pages/0") else {
            return Ok(FilePage {
                exists: false,
                file: None,
            });
        };
        if page
            .get("missing")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Ok(FilePage {
                exists: false,
                file: None,
            });
        }
        let file = page
            .get("imageinfo")
            .and_then(Value::as_array)
            .and_then(|info| info.first())
            .and_then(|info| parse_file_info(info, filename.as_str()));
        Ok(FilePage { exists: true, file })
    }

    /// Current files with the same SHA-1, under any name.
    pub async fn files_with_sha1(&self, sha1: Sha1Hash) -> Result<Vec<FileInfo>, WikimediaError> {
        let hex = sha1.to_hex();
        let answer = self
            .get(&[
                ("action", "query"),
                ("list", "allimages"),
                ("aisha1", &hex),
                ("aiprop", "url|size|sha1|mime"),
                ("ailimit", MAX_RESULTS),
            ])
            .await?;
        Ok(file_list(answer.pointer("/query/allimages")))
    }

    /// Deleted files with the same SHA-1, under any name.
    pub async fn deleted_files_with_sha1(
        &self,
        sha1: Sha1Hash,
    ) -> Result<Vec<FileInfo>, WikimediaError> {
        let hex = sha1.to_hex();
        let answer = self
            .get(&[
                ("action", "query"),
                ("list", "filearchive"),
                ("fasha1", &hex),
                ("faprop", "sha1|size"),
                ("falimit", MAX_RESULTS),
            ])
            .await?;
        Ok(file_list(answer.pointer("/query/filearchive")))
    }

    /// Whether the target name appears in the deletion log.
    pub async fn was_deleted(&self, filename: &FileName) -> Result<bool, WikimediaError> {
        let title = format!("File:{}", filename);
        let answer = self
            .get(&[
                ("action", "query"),
                ("list", "logevents"),
                ("letype", "delete"),
                ("leaction", "delete/delete"),
                ("letitle", &title),
                ("lelimit", "1"),
            ])
            .await?;
        Ok(answer
            .pointer("/query/logevents")
            .and_then(Value::as_array)
            .is_some_and(|events| !events.is_empty()))
    }

    /// The wiki's upload-size and chunk-size limits.
    pub async fn site_info(&self) -> Result<CommonsSiteInfo, WikimediaError> {
        let answer = self
            .get(&[
                ("action", "query"),
                ("meta", "siteinfo"),
                ("siprop", "general"),
            ])
            .await?;
        let general = answer.pointer("/query/general");
        Ok(CommonsSiteInfo {
            max_upload_size: general.and_then(|general| as_u64(general.get("maxuploadsize"))),
            min_upload_chunk_size: general
                .and_then(|general| as_u64(general.get("minuploadchunksize"))),
        })
    }

    /// Runs the checks a transfer tool needs before uploading `filename`.
    pub async fn check(
        &self,
        filename: &FileName,
        facts: &FileFacts,
    ) -> Result<CommonsCheck, WikimediaError> {
        let page = self.file_page(filename).await?;
        let duplicates = self.files_with_sha1(facts.sha1).await?;
        let deleted_duplicates = self.deleted_files_with_sha1(facts.sha1).await?;
        let was_deleted = self.was_deleted(filename).await?;
        let site = self.site_info().await?;
        Ok(CommonsCheck {
            page_exists: page.exists,
            existing_file: page.file,
            duplicates,
            deleted_duplicates,
            was_deleted,
            max_upload_size: site.max_upload_size,
            size: facts.size,
            sha1: facts.sha1,
        })
    }

    async fn get(&self, params: &[(&str, &str)]) -> Result<Value, WikimediaError> {
        let response = self
            .client
            .get(self.api_url.clone())
            .query(params)
            .query(&[("format", "json"), ("formatversion", "2")])
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        let value: Value = match serde_json::from_str(&body) {
            Ok(value) => value,
            Err(source) => {
                return if status.is_success() {
                    Err(WikimediaError::Json {
                        source,
                        body: snippet(&body),
                    })
                } else {
                    Err(WikimediaError::HttpStatus {
                        status,
                        body: snippet(&body),
                    })
                };
            }
        };
        if let Some(error) = value.get("error") {
            let mut error = ApiError::from_value(error);
            if error.info.is_none() {
                error.info = value
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            return Err(WikimediaError::Api(error));
        }
        if !status.is_success() {
            return Err(WikimediaError::HttpStatus {
                status,
                body: snippet(&body),
            });
        }
        Ok(value)
    }
}

fn file_list(value: Option<&Value>) -> Vec<FileInfo> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let fallback = item
                        .get("name")
                        .and_then(Value::as_str)
                        .or_else(|| item.get("title").and_then(Value::as_str))
                        .unwrap_or_default();
                    parse_file_info(item, fallback)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_file_info(value: &Value, fallback_name: &str) -> Option<FileInfo> {
    let size = as_u64(value.get("size"))?;
    let sha1 = value
        .get("sha1")
        .and_then(Value::as_str)
        .and_then(|sha1| Sha1Hash::from_hex(sha1).ok())?;
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("title")
                .and_then(Value::as_str)
                .map(|title| title.trim_start_matches("File:"))
        })
        .unwrap_or(fallback_name)
        .to_string();
    Some(FileInfo {
        name,
        size,
        sha1,
        mime: value
            .get("mime")
            .and_then(Value::as_str)
            .map(str::to_string),
        url: value.get("url").and_then(Value::as_str).map(str::to_string),
    })
}

fn as_u64(value: Option<&Value>) -> Option<u64> {
    match value? {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.parse().ok(),
        _ => None,
    }
}

fn snippet(body: &str) -> String {
    const LIMIT: usize = 500;
    let mut text: String = body.chars().take(LIMIT).collect();
    if text.len() < body.len() {
        text.push('…');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "a84d2a201166a7db1d3fdba63dece1b1d068e2f3";
    const BASE36: &str = "jnqovekis95cwt259scunaikycp1wc3";

    #[test]
    fn sha1_hex_and_base36_round_trip() {
        let sha1 = Sha1Hash::from_hex(HEX).expect("hex");
        assert_eq!(sha1.to_hex(), HEX);
        assert_eq!(sha1.to_base36(), BASE36);
        assert_eq!(Sha1Hash::from_base36(BASE36).expect("base36"), sha1);
    }

    #[test]
    fn base36_is_zero_padded_to_31_digits() {
        let sha1 = Sha1Hash::from_hex("0000000000000000000000000000000000000001").expect("hex");
        assert_eq!(sha1.to_base36(), format!("{}1", "0".repeat(30)));
        assert_eq!(
            Sha1Hash::from_base36(&sha1.to_base36()).expect("base36"),
            sha1
        );
    }

    #[test]
    fn sha1_rejects_invalid_values() {
        assert!(Sha1Hash::from_hex("not a hash").is_err());
        assert!(Sha1Hash::from_hex(&"g".repeat(40)).is_err());
        assert!(Sha1Hash::from_base36("not/a/hash").is_err());
    }

    #[test]
    fn facts_for_bytes() {
        let facts = bytes_facts(b"test bytes");
        assert_eq!(facts.size, 10);
        assert_eq!(facts.sha1.to_hex().len(), 40);
    }

    #[test]
    fn parses_file_info_from_api_shapes() {
        let file = parse_file_info(
            &serde_json::json!({
                "size": "123",
                "sha1": HEX,
                "mime": "image/jpeg",
                "url": "https://example.org/a.jpg"
            }),
            "Example.jpg",
        )
        .expect("file");
        assert_eq!(file.name, "Example.jpg");
        assert_eq!(file.size, 123);
        assert_eq!(file.sha1.to_hex(), HEX);
        assert_eq!(file.mime.as_deref(), Some("image/jpeg"));
    }

    #[test]
    fn check_helpers() {
        let check = CommonsCheck {
            page_exists: false,
            existing_file: None,
            duplicates: vec![],
            deleted_duplicates: vec![],
            was_deleted: false,
            max_upload_size: Some(100),
            size: 100,
            sha1: Sha1Hash::from_hex(HEX).expect("hex"),
        };
        assert!(check.is_free());
        assert!(check.size_allowed());
        assert!(!check.needs_review());
    }
}
