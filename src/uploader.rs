//! Uploading a file to a MediaWiki wiki.

use crate::credentials::Credentials;
use crate::error::{ApiError, UploadError, UploadFailure};
use crate::filename::FileName;
use reqwest::header::AUTHORIZATION;
use reqwest::multipart::{Form, Part};
use reqwest::{Client, RequestBuilder};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use url::Url;

/// The Action API of Wikimedia Commons.
pub const COMMONS_API: &str = "https://commons.wikimedia.org/w/api.php";

/// The chunk size used by [`UploadRequest::chunked`].
///
/// MediaWiki requires non-final chunks to be at least 1 KiB on Wikimedia
/// wikis; 5 MiB keeps the number of requests moderate while staying far above
/// that minimum.
pub const DEFAULT_CHUNK_SIZE: u64 = 5 * 1024 * 1024;

/// A single upload request.
///
/// The filename is normalized by [`FileName`]. Everything else is optional:
/// by default no warnings are ignored and no warnings block the upload, so
/// the wiki is free to report them and the caller can decide what to do.
#[derive(Debug, Clone)]
pub struct UploadRequest {
    filename: FileName,
    text: String,
    comment: String,
    ignore_warnings: bool,
    blocking_warnings: Vec<String>,
    chunk_size: Option<u64>,
    tags: Vec<String>,
}

impl UploadRequest {
    /// Creates a request for `filename` with an empty description.
    pub fn new(filename: FileName) -> Self {
        Self {
            filename,
            text: String::new(),
            comment: String::new(),
            ignore_warnings: false,
            blocking_warnings: Vec::new(),
            chunk_size: None,
            tags: Vec::new(),
        }
    }

    /// Sets the initial wikitext of the file description page.
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text = text.into();
        self
    }

    /// Sets the upload comment, which also appears in the upload log.
    pub fn comment(mut self, comment: impl Into<String>) -> Self {
        self.comment = comment.into();
        self
    }

    /// Whether to pass `ignorewarnings` to the wiki.
    ///
    /// This defaults to `false`. Ignoring warnings can overwrite an existing
    /// file, so enable it only when the caller has checked the target name or
    /// when warnings are inspected with [`Self::blocking_warnings`].
    pub fn ignore_warnings(mut self, ignore: bool) -> Self {
        self.ignore_warnings = ignore;
        self
    }

    /// Warning keys that must still stop the upload when warnings are being
    /// ignored.
    ///
    /// When this is non-empty, the first upload request is made without
    /// `ignorewarnings`. If the wiki reports only cosmetic warnings, the
    /// stashed file is published; if one of these keys is present, the upload
    /// is refused and the warnings are returned in [`UploadFailure`].
    pub fn blocking_warnings<I, S>(mut self, warnings: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for warning in warnings {
            self.add_blocking_warning(warning.as_ref().to_string());
        }
        self
    }

    /// Refuses byte-identical re-uploads, as the Flickr2Commons tool does.
    ///
    /// This adds the `duplicate` and `duplicate-archive` warnings to the
    /// blocking list.
    pub fn block_duplicates(mut self) -> Self {
        self.add_blocking_warning("duplicate".to_string());
        self.add_blocking_warning("duplicate-archive".to_string());
        self
    }

    /// Change tags to apply to the upload log entry and file version.
    ///
    /// MediaWiki expects a `|`-separated list. Empty values and duplicates are
    /// dropped. The tags must already exist on the wiki.
    pub fn tags<I, S>(mut self, tags: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for tag in tags {
            let tag = tag.as_ref().trim();
            if !tag.is_empty() && !self.tags.iter().any(|existing| existing.as_str() == tag) {
                self.tags.push(tag.to_string());
            }
        }
        self
    }

    /// Uploads files larger than [`DEFAULT_CHUNK_SIZE`] in chunks.
    ///
    /// Smaller files use a single request. Use [`Self::chunk_size`] to choose
    /// a different threshold and chunk size.
    pub fn chunked(mut self) -> Self {
        self.chunk_size = Some(DEFAULT_CHUNK_SIZE);
        self
    }

    /// Uploads files larger than `bytes` in chunks of at most `bytes`.
    ///
    /// This is also the threshold: a file no larger than `bytes` is uploaded
    /// in one request. A value of `0` disables chunking. The wiki's minimum
    /// chunk size still applies, so do not choose less than a few KiB.
    pub fn chunk_size(mut self, bytes: u64) -> Self {
        self.chunk_size = (bytes > 0).then_some(bytes);
        self
    }

    /// The normalized target file name.
    pub fn filename(&self) -> &FileName {
        &self.filename
    }

    fn add_blocking_warning(&mut self, warning: String) {
        if !warning.is_empty() && !self.blocking_warnings.contains(&warning) {
            self.blocking_warnings.push(warning);
        }
    }
}

/// A successful upload.
#[derive(Debug, Clone)]
pub struct UploadSuccess {
    /// The file name the wiki stored, which may have been normalized further.
    pub filename: String,

    /// Any warnings the wiki returned with the successful upload.
    pub warnings: BTreeMap<String, Value>,
}

/// An OAuth-authenticated client for uploading files.
#[derive(Clone)]
pub struct Uploader {
    api_url: Url,
    client: Client,
    credentials: Credentials,
}

impl std::fmt::Debug for Uploader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Uploader")
            .field("api_url", &self.api_url)
            .field("credentials", &self.credentials)
            .finish_non_exhaustive()
    }
}

impl Uploader {
    /// Creates an uploader with a dedicated HTTP client.
    ///
    /// The client identifies itself with `user_agent` and does not follow
    /// redirects, so a redirect cannot turn an authenticated POST into
    /// something else. There is no overall or read timeout, because a large
    /// upload may legitimately take a long time; use [`Self::with_client`] to
    /// supply a client with timeouts suited to the tool.
    pub fn new(
        api_url: impl AsRef<str>,
        user_agent: impl AsRef<str>,
        credentials: Credentials,
    ) -> Result<Self, UploadError> {
        let client = Client::builder()
            .user_agent(user_agent.as_ref())
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Self::with_client(api_url, client, credentials)
    }

    /// Creates an uploader around an existing HTTP client.
    ///
    /// The client must carry a User-Agent header, as Wikimedia's policy
    /// requires. This is also the way to use session cookies or a custom TLS
    /// or proxy configuration.
    pub fn with_client(
        api_url: impl AsRef<str>,
        client: Client,
        credentials: Credentials,
    ) -> Result<Self, UploadError> {
        let api_url = Url::parse(api_url.as_ref())?;
        if !matches!(api_url.scheme(), "http" | "https") {
            return Err(UploadError::InvalidScheme);
        }
        if api_url.host_str().is_none() {
            return Err(UploadError::MissingHost);
        }
        Ok(Self {
            api_url,
            client,
            credentials,
        })
    }

    /// Creates an uploader for Wikimedia Commons.
    pub fn commons(
        user_agent: impl AsRef<str>,
        credentials: Credentials,
    ) -> Result<Self, UploadError> {
        Self::new(COMMONS_API, user_agent, credentials)
    }

    /// The Action API URL this uploader talks to.
    pub fn api_url(&self) -> &Url {
        &self.api_url
    }

    /// Uploads an in-memory file.
    pub async fn upload_bytes(
        &self,
        bytes: impl Into<Vec<u8>>,
        request: UploadRequest,
    ) -> Result<UploadSuccess, UploadError> {
        let bytes = bytes.into();
        let size = bytes.len() as u64;
        if let Some(chunk_size) = request.chunk_size.filter(|chunk_size| size > *chunk_size) {
            return self
                .upload_bytes_chunked(bytes, size, chunk_size, request)
                .await;
        }
        let part = Part::bytes(bytes).file_name(request.filename.as_str().to_string());
        self.upload_part(&request, part).await
    }

    /// Uploads a file from disk, streaming it into the multipart request.
    ///
    /// Files larger than the request's `chunk_size` are uploaded in chunks
    /// instead.
    pub async fn upload_file(
        &self,
        path: impl AsRef<Path>,
        request: UploadRequest,
    ) -> Result<UploadSuccess, UploadError> {
        let path = path.as_ref();
        let metadata = tokio::fs::metadata(path)
            .await
            .map_err(|source| file_error(path, source))?;
        if !metadata.is_file() {
            return Err(UploadError::NotAFile {
                path: path.to_path_buf(),
            });
        }
        let size = metadata.len();
        if let Some(chunk_size) = request.chunk_size.filter(|chunk_size| size > *chunk_size) {
            return self
                .upload_file_chunked(path, size, chunk_size, request)
                .await;
        }
        let part = Part::file(path)
            .await
            .map_err(|source| file_error(path, source))?;
        let part = part.file_name(request.filename.as_str().to_string());
        self.upload_part(&request, part).await
    }

    /// Publishes a file that MediaWiki left in the upload stash.
    ///
    /// An [`UploadFailure`] carries the `file_key` of such a file. A caller
    /// that has inspected the warnings and decided to publish anyway can pass
    /// it here together with the request used for the upload.
    pub async fn publish_stashed(
        &self,
        request: &UploadRequest,
        file_key: &str,
    ) -> Result<UploadSuccess, UploadError> {
        let token = self.csrf_token().await?;
        let status = self.stashed_upload(request, &token, file_key, true).await?;
        upload_success(status, request)
    }

    async fn upload_part(
        &self,
        request: &UploadRequest,
        part: Part,
    ) -> Result<UploadSuccess, UploadError> {
        let token = self.csrf_token().await?;
        let fields = upload_fields(request, &token);

        let mut form = Form::new();
        for (key, value) in &fields {
            form = form.text(key.clone(), value.clone());
        }
        form = form.part("file", part);

        let builder = self.client.post(self.api_url.clone()).multipart(form);
        let builder = self.authorize(builder, "POST", &[])?;
        let answer = self.send(builder).await?;
        let status = parse_upload(&answer)?;

        if status.result == "Warning" && !request.blocking_warnings.is_empty() {
            if has_blocking_warning(request, &status) {
                return Err(UploadError::Upload(upload_failure(status)));
            }
            if request.ignore_warnings {
                let file_key = status.filekey.clone().ok_or(UploadError::MissingFileKey)?;
                let status = self
                    .stashed_upload(request, &token, &file_key, true)
                    .await?;
                return upload_success(status, request);
            }
        }
        upload_success(status, request)
    }

    async fn upload_bytes_chunked(
        &self,
        bytes: Vec<u8>,
        size: u64,
        chunk_size: u64,
        request: UploadRequest,
    ) -> Result<UploadSuccess, UploadError> {
        let token = self.csrf_token().await?;
        let mut upload = ChunkUpload::new(self, &request, token, size);
        let chunk_len = usize::try_from(chunk_size).unwrap_or(usize::MAX).max(1);
        for chunk in bytes.chunks(chunk_len) {
            upload.send(chunk.to_vec()).await?;
        }
        upload.finish().await
    }

    async fn upload_file_chunked(
        &self,
        path: &Path,
        size: u64,
        chunk_size: u64,
        request: UploadRequest,
    ) -> Result<UploadSuccess, UploadError> {
        let mut file = tokio::fs::File::open(path)
            .await
            .map_err(|source| file_error(path, source))?;
        let token = self.csrf_token().await?;
        let mut upload = ChunkUpload::new(self, &request, token, size);
        let mut offset = 0u64;
        while offset < size {
            let this_len = (size - offset).min(chunk_size) as usize;
            let mut buffer = vec![0u8; this_len];
            file.read_exact(&mut buffer)
                .await
                .map_err(|source| file_error(path, source))?;
            upload.send(buffer).await?;
            offset += this_len as u64;
        }
        upload.finish().await
    }

    async fn commit_stashed(
        &self,
        request: &UploadRequest,
        token: &str,
        initial_file_key: &str,
    ) -> Result<UploadSuccess, UploadError> {
        let mut file_key = initial_file_key.to_string();
        let mut status = self
            .stashed_upload(
                request,
                token,
                &file_key,
                request.ignore_warnings && request.blocking_warnings.is_empty(),
            )
            .await?;

        if status.result == "Warning" && !request.blocking_warnings.is_empty() {
            if has_blocking_warning(request, &status) {
                return Err(UploadError::Upload(upload_failure(status)));
            }
            if request.ignore_warnings {
                if let Some(key) = status.filekey.clone() {
                    file_key = key;
                }
                status = self.stashed_upload(request, token, &file_key, true).await?;
            }
        }
        upload_success(status, request)
    }

    async fn stashed_upload(
        &self,
        request: &UploadRequest,
        token: &str,
        file_key: &str,
        ignore_warnings: bool,
    ) -> Result<UploadStatus, UploadError> {
        let fields = stashed_fields(request, token, file_key, ignore_warnings);
        let answer = self.post_form(&fields).await?;
        parse_upload(&answer)
    }

    async fn csrf_token(&self) -> Result<String, UploadError> {
        let params = vec![
            ("action".to_string(), "query".to_string()),
            ("meta".to_string(), "tokens".to_string()),
            ("type".to_string(), "csrf".to_string()),
            ("format".to_string(), "json".to_string()),
        ];
        let answer = self.post_form(&params).await?;
        answer
            .pointer("/query/tokens/csrftoken")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| UploadError::MissingToken {
                body: answer.to_string(),
            })
    }

    async fn post_form(&self, params: &[(String, String)]) -> Result<Value, UploadError> {
        let builder = self.client.post(self.api_url.clone()).form(params);
        let builder = self.authorize(builder, "POST", params)?;
        self.send(builder).await
    }

    fn authorize(
        &self,
        builder: RequestBuilder,
        method: &str,
        params: &[(String, String)],
    ) -> Result<RequestBuilder, UploadError> {
        Ok(match &self.credentials {
            Credentials::OAuth1(oauth) => builder.header(
                AUTHORIZATION,
                oauth.authorization_header(method, self.api_url.as_str(), params)?,
            ),
            Credentials::Bearer(token) => builder.bearer_auth(token),
            Credentials::None => builder,
        })
    }

    async fn send(&self, builder: RequestBuilder) -> Result<Value, UploadError> {
        let response = builder.send().await?;
        let status = response.status();
        let body = response.text().await?;
        let value: Value = match serde_json::from_str(&body) {
            Ok(value) => value,
            Err(source) => {
                return if status.is_success() {
                    Err(UploadError::Json {
                        source,
                        body: snippet(&body),
                    })
                } else {
                    Err(UploadError::HttpStatus {
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
            return Err(UploadError::Api(error));
        }
        if !status.is_success() {
            return Err(UploadError::HttpStatus {
                status,
                body: snippet(&body),
            });
        }
        Ok(value)
    }
}

/// A chunked upload in progress.
struct ChunkUpload<'a> {
    uploader: &'a Uploader,
    request: &'a UploadRequest,
    token: String,
    size: u64,
    offset: u64,
    file_key: Option<String>,
    index: usize,
}

impl<'a> ChunkUpload<'a> {
    fn new(uploader: &'a Uploader, request: &'a UploadRequest, token: String, size: u64) -> Self {
        Self {
            uploader,
            request,
            token,
            size,
            offset: 0,
            file_key: None,
            index: 0,
        }
    }

    async fn upload_chunk(&self, chunk: Vec<u8>) -> Result<UploadStatus, UploadError> {
        let mut fields = vec![
            ("action".to_string(), "upload".to_string()),
            ("format".to_string(), "json".to_string()),
            ("assert".to_string(), "user".to_string()),
            (
                "filename".to_string(),
                self.request.filename.as_str().to_string(),
            ),
            ("filesize".to_string(), self.size.to_string()),
            ("offset".to_string(), self.offset.to_string()),
            ("stash".to_string(), "1".to_string()),
            ("ignorewarnings".to_string(), "1".to_string()),
            ("token".to_string(), self.token.clone()),
        ];
        if let Some(file_key) = self.file_key.as_deref() {
            fields.push(("filekey".to_string(), file_key.to_string()));
        }

        let part = Part::bytes(chunk).file_name(format!("chunk-{}", self.index));
        let mut form = Form::new();
        for (key, value) in &fields {
            form = form.text(key.clone(), value.clone());
        }
        form = form.part("chunk", part);

        let builder = self
            .uploader
            .client
            .post(self.uploader.api_url.clone())
            .multipart(form);
        let builder = self.uploader.authorize(builder, "POST", &[])?;
        let answer = self.uploader.send(builder).await?;
        parse_upload(&answer)
    }

    async fn send(&mut self, chunk: Vec<u8>) -> Result<(), UploadError> {
        let chunk_len = chunk.len() as u64;
        let is_last = self.offset + chunk_len >= self.size;
        let status = self.upload_chunk(chunk).await?;

        if !is_last && status.result != "Continue" {
            return Err(UploadError::UnexpectedChunkResult {
                result: status.result,
            });
        }
        if is_last && !matches!(status.result.as_str(), "Success" | "Continue" | "Warning") {
            return Err(UploadError::UnexpectedChunkResult {
                result: status.result,
            });
        }

        let expected_offset = self.offset + chunk_len;
        if let Some(offset) = status.offset
            && offset != expected_offset
        {
            return Err(UploadError::UnexpectedChunkOffset {
                expected: expected_offset,
                actual: offset,
            });
        }

        self.file_key = Some(
            status
                .filekey
                .clone()
                .or_else(|| self.file_key.clone())
                .ok_or(UploadError::MissingFileKey)?,
        );
        self.offset = expected_offset;
        self.index += 1;
        Ok(())
    }

    async fn finish(self) -> Result<UploadSuccess, UploadError> {
        let file_key = self.file_key.ok_or(UploadError::MissingFileKey)?;
        self.uploader
            .commit_stashed(self.request, &self.token, &file_key)
            .await
    }
}

/// The parameters shared by every upload request.
fn base_fields(request: &UploadRequest) -> Vec<(String, String)> {
    let mut fields = vec![
        ("action".to_string(), "upload".to_string()),
        ("format".to_string(), "json".to_string()),
        ("assert".to_string(), "user".to_string()),
        (
            "filename".to_string(),
            request.filename.as_str().to_string(),
        ),
        ("comment".to_string(), request.comment.clone()),
        ("text".to_string(), request.text.clone()),
    ];
    if !request.tags.is_empty() {
        fields.push(("tags".to_string(), request.tags.join("|")));
    }
    fields
}

/// The parameters MediaWiki needs for a first, unchunked upload request.
fn upload_fields(request: &UploadRequest, token: &str) -> Vec<(String, String)> {
    let mut fields = base_fields(request);
    fields.push(("token".to_string(), token.to_string()));
    if request.ignore_warnings && request.blocking_warnings.is_empty() {
        fields.push(("ignorewarnings".to_string(), "1".to_string()));
    }
    fields
}

/// The parameters for publishing a file that MediaWiki stashed, either after
/// warnings or after a chunked upload. The file is identified by `file_key`;
/// there is no file part.
fn stashed_fields(
    request: &UploadRequest,
    token: &str,
    file_key: &str,
    ignore_warnings: bool,
) -> Vec<(String, String)> {
    let mut fields = base_fields(request);
    fields.push(("token".to_string(), token.to_string()));
    fields.push(("filekey".to_string(), file_key.to_string()));
    if ignore_warnings {
        fields.push(("ignorewarnings".to_string(), "1".to_string()));
    }
    fields
}

#[derive(Debug, Deserialize)]
struct UploadStatus {
    #[serde(default)]
    result: String,
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    filekey: Option<String>,
    #[serde(default)]
    offset: Option<u64>,
    #[serde(default)]
    warnings: BTreeMap<String, Value>,
}

fn parse_upload(answer: &Value) -> Result<UploadStatus, UploadError> {
    let upload = answer
        .get("upload")
        .filter(|upload| !upload.is_null())
        .ok_or_else(|| UploadError::MissingUpload {
            body: answer.to_string(),
        })?;
    serde_json::from_value(upload.clone()).map_err(|source| UploadError::MalformedUpload {
        source,
        value: upload.clone(),
    })
}

fn upload_success(
    status: UploadStatus,
    request: &UploadRequest,
) -> Result<UploadSuccess, UploadError> {
    if status.result != "Success" {
        return Err(UploadError::Upload(upload_failure(status)));
    }
    Ok(UploadSuccess {
        filename: status
            .filename
            .unwrap_or_else(|| request.filename.as_str().to_string()),
        warnings: status.warnings,
    })
}

fn upload_failure(status: UploadStatus) -> UploadFailure {
    UploadFailure {
        result: status.result,
        warnings: status.warnings,
        file_key: status.filekey,
    }
}

fn has_blocking_warning(request: &UploadRequest, status: &UploadStatus) -> bool {
    request
        .blocking_warnings
        .iter()
        .any(|key| warning_is_present(status.warnings.get(key)))
}

/// Warnings that are present and say something. MediaWiki represents a
/// duplicate list as an array; an empty list means there was no duplicate.
/// A key that is present otherwise counts as a warning, as in the PHP tools.
fn warning_is_present(value: Option<&Value>) -> bool {
    match value {
        None => false,
        Some(Value::Array(values)) => !values.is_empty(),
        Some(Value::Object(values)) => !values.is_empty(),
        Some(_) => true,
    }
}

fn file_error(path: &Path, source: std::io::Error) -> UploadError {
    UploadError::File {
        path: path.to_path_buf(),
        source,
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
    use crate::filename::FileName;
    use serde_json::json;

    fn request() -> UploadRequest {
        UploadRequest::new(FileName::new("Example.jpg").expect("name"))
    }

    #[test]
    fn ignore_warnings_without_checks_is_sent() {
        let fields = upload_fields(&request().ignore_warnings(true), "token");
        assert!(fields.contains(&("ignorewarnings".to_string(), "1".to_string())));
    }

    #[test]
    fn blocking_warnings_prevent_ignorewarnings_in_the_first_request() {
        let fields = upload_fields(&request().ignore_warnings(true).block_duplicates(), "token");
        assert!(!fields.iter().any(|(key, _)| key == "ignorewarnings"));
    }

    #[test]
    fn stashed_fields_carry_the_file_key_and_no_file() {
        let fields = stashed_fields(&request(), "token", "file-key", true);
        assert!(!fields.iter().any(|(key, _)| key == "file"));
        assert!(fields.contains(&("filekey".to_string(), "file-key".to_string())));
        assert!(fields.contains(&("ignorewarnings".to_string(), "1".to_string())));
    }

    #[test]
    fn change_tags_are_sent_once() {
        let request = request().tags(["flickr2commons", "flickr2commons", ""]);
        assert_eq!(request.tags, vec!["flickr2commons".to_string()]);
        let fields = upload_fields(&request, "token");
        assert!(fields.contains(&("tags".to_string(), "flickr2commons".to_string())));
        let fields = stashed_fields(&request, "token", "file-key", false);
        assert!(fields.contains(&("tags".to_string(), "flickr2commons".to_string())));
    }

    #[test]
    fn no_tags_means_no_parameter() {
        let fields = upload_fields(&request(), "token");
        assert!(!fields.iter().any(|(key, _)| key == "tags"));
    }

    #[test]
    fn accepts_warning_slices() {
        let request = request().blocking_warnings(["duplicate", "duplicate"]);
        assert_eq!(request.blocking_warnings, vec!["duplicate".to_string()]);
    }

    #[test]
    fn chunk_size_zero_disables_chunking() {
        assert!(request().chunk_size(0).chunk_size.is_none());
        assert_eq!(request().chunked().chunk_size, Some(DEFAULT_CHUNK_SIZE));
    }

    #[test]
    fn warning_values_are_present_as_in_php() {
        assert!(!warning_is_present(None));
        assert!(!warning_is_present(Some(&json!([]))));
        assert!(!warning_is_present(Some(&json!({}))));
        assert!(warning_is_present(Some(&json!(null))));
        assert!(warning_is_present(Some(&json!(""))));
        assert!(warning_is_present(Some(&json!(false))));
        assert!(warning_is_present(Some(&json!(["Other.jpg"]))));
        assert!(warning_is_present(Some(&json!("Other.jpg"))));
        assert!(warning_is_present(Some(&json!(true))));
    }
}
