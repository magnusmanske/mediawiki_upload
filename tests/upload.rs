#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use mediawiki_upload::{Credentials, FileName, UploadError, UploadRequest, Uploader};
use wiremock::matchers::{body_string_contains, header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn uploader(server: &MockServer, credentials: Credentials) -> Uploader {
    Uploader::new(
        format!("{}/w/api.php", server.uri()),
        "mediawiki_upload-test/1.0 (nobody@example.org)",
        credentials,
    )
    .expect("uploader")
}

fn request() -> UploadRequest {
    UploadRequest::new(FileName::new("Example image.jpg").expect("file name"))
}

async fn mount_token(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/w/api.php"))
        .and(body_string_contains("meta=tokens"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "query": {"tokens": {"csrftoken": "csrf-token"}}
        })))
        .expect(1)
        .mount(server)
        .await;
}

fn bodies(requests: Vec<wiremock::Request>) -> Vec<String> {
    requests
        .into_iter()
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .collect()
}

const TEST_CHUNK_SIZE: u64 = 5;
const TEST_FILE: &[u8] = b"hello world!";

/// Reads one field from a `multipart/form-data` body.
fn multipart_field(body: &str, name: &str) -> Option<String> {
    let marker = format!("name=\"{name}\"");
    let start = body.find(&marker)?;
    let rest = &body[start + marker.len()..];
    let value_start = rest.find("\r\n\r\n")? + 4;
    let rest = &rest[value_start..];
    let value_end = rest.find("\r\n--").unwrap_or(rest.len());
    Some(rest[..value_end].to_string())
}

async fn mount_chunks(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/w/api.php"))
        .and(body_string_contains("name=\"chunk\""))
        .respond_with(|request: &wiremock::Request| {
            let body = String::from_utf8_lossy(&request.body);
            let offset = multipart_field(&body, "offset")
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or_default();
            let response = if offset + TEST_CHUNK_SIZE >= TEST_FILE.len() as u64 {
                serde_json::json!({"upload": {"result": "Success", "filekey": "stash-key"}})
            } else {
                serde_json::json!({
                    "upload": {
                        "result": "Continue",
                        "offset": offset + TEST_CHUNK_SIZE,
                        "filekey": "stash-key"
                    }
                })
            };
            ResponseTemplate::new(200).set_body_json(response)
        })
        .expect(3)
        .mount(server)
        .await;
}

async fn mount_commit_success(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/w/api.php"))
        .and(body_string_contains("comment="))
        .and(body_string_contains("filekey=stash-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "upload": {"result": "Success", "filename": "Example_image.jpg"}
        })))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn uploads_bytes_with_oauth1() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("POST"))
        .and(path("/w/api.php"))
        .and(body_string_contains("name=\"filename\""))
        .and(header_exists("authorization"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "upload": {"result": "Success", "filename": "Example_image.jpg"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let uploader = uploader(
        &server,
        Credentials::oauth1("consumer", "consumer-secret", "token", "token-secret"),
    );
    let result = uploader
        .upload_bytes(b"test bytes".to_vec(), request())
        .await
        .expect("upload");

    assert_eq!(result.filename, "Example_image.jpg");
    let bodies = bodies(server.received_requests().await.unwrap_or_default());
    assert!(bodies.iter().any(|body| body.contains("name=\"filename\"")));
    assert!(bodies.iter().any(|body| body.contains("Example_image.jpg")));
}

#[tokio::test]
async fn uploads_a_file_with_a_bearer_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/w/api.php"))
        .and(header("authorization", "Bearer bearer-token"))
        .and(body_string_contains("meta=tokens"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "query": {"tokens": {"csrftoken": "csrf-token"}}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/w/api.php"))
        .and(header("authorization", "Bearer bearer-token"))
        .and(body_string_contains("name=\"file\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "upload": {"result": "Success", "filename": "Example_image.jpg"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let file = tempfile::NamedTempFile::new().expect("temp file");
    std::fs::write(file.path(), b"file bytes").expect("write");
    let uploader = uploader(&server, Credentials::bearer("bearer-token"));
    let result = uploader
        .upload_file(file.path(), request())
        .await
        .expect("upload");

    assert_eq!(result.filename, "Example_image.jpg");
}

#[tokio::test]
async fn blocking_warning_stops_the_upload_and_leaves_the_stash() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("POST"))
        .and(path("/w/api.php"))
        .and(body_string_contains("name=\"filename\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "upload": {
                "result": "Warning",
                "warnings": {
                    "duplicate": ["Other.jpg"],
                    "was-deleted": "Example_image.jpg"
                },
                "filekey": "stash-key"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let uploader = uploader(&server, Credentials::bearer("bearer-token"));
    let error = uploader
        .upload_bytes(
            b"test bytes".to_vec(),
            request().ignore_warnings(true).block_duplicates(),
        )
        .await
        .expect_err("duplicate must be refused");

    match error {
        UploadError::Upload(failure) => {
            assert_eq!(failure.result, "Warning");
            assert!(failure.warnings.contains_key("duplicate"));
            assert_eq!(failure.file_key.as_deref(), Some("stash-key"));
            assert!(failure.to_string().contains("Other.jpg"));
        }
        other => panic!("unexpected error: {other:?}"),
    }

    let bodies = bodies(server.received_requests().await.unwrap_or_default());
    assert!(!bodies.iter().any(|body| body.contains("filekey=stash-key")));
}

#[tokio::test]
async fn cosmetic_warning_is_published_when_duplicates_are_blocked() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("POST"))
        .and(path("/w/api.php"))
        .and(body_string_contains("name=\"filename\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "upload": {
                "result": "Warning",
                "warnings": {"was-deleted": "Example_image.jpg"},
                "filekey": "stash-key"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/w/api.php"))
        .and(body_string_contains("filekey=stash-key"))
        .and(body_string_contains("ignorewarnings=1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "upload": {"result": "Success", "filename": "Example_image.jpg"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let uploader = uploader(&server, Credentials::bearer("bearer-token"));
    let result = uploader
        .upload_bytes(
            b"test bytes".to_vec(),
            request().ignore_warnings(true).block_duplicates(),
        )
        .await
        .expect("publish");

    assert_eq!(result.filename, "Example_image.jpg");
    let bodies = bodies(server.received_requests().await.unwrap_or_default());
    assert!(bodies.iter().any(|body| body.contains("filekey=stash-key")));
}

#[tokio::test]
async fn uploads_bytes_in_chunks() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_chunks(&server).await;
    mount_commit_success(&server).await;

    let uploader = uploader(&server, Credentials::bearer("bearer-token"));
    let result = uploader
        .upload_bytes(TEST_FILE.to_vec(), request().chunk_size(TEST_CHUNK_SIZE))
        .await
        .expect("chunked upload");

    assert_eq!(result.filename, "Example_image.jpg");
    let bodies = bodies(server.received_requests().await.unwrap_or_default());
    assert_eq!(
        bodies
            .iter()
            .filter(|body| body.contains("name=\"chunk\""))
            .count(),
        3
    );
}

#[tokio::test]
async fn uploads_a_file_in_chunks() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_chunks(&server).await;
    mount_commit_success(&server).await;

    let file = tempfile::NamedTempFile::new().expect("temp file");
    std::fs::write(file.path(), TEST_FILE).expect("write");
    let uploader = uploader(&server, Credentials::bearer("bearer-token"));
    let result = uploader
        .upload_file(file.path(), request().chunk_size(TEST_CHUNK_SIZE))
        .await
        .expect("chunked upload");

    assert_eq!(result.filename, "Example_image.jpg");
    let bodies = bodies(server.received_requests().await.unwrap_or_default());
    assert_eq!(
        bodies
            .iter()
            .filter(|body| body.contains("name=\"chunk\""))
            .count(),
        3
    );
}

#[tokio::test]
async fn chunked_upload_blocks_a_duplicate_at_commit_time() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_chunks(&server).await;
    Mock::given(method("POST"))
        .and(path("/w/api.php"))
        .and(body_string_contains("comment="))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "upload": {
                "result": "Warning",
                "warnings": {"duplicate": ["Other.jpg"]},
                "filekey": "stash-key"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let uploader = uploader(&server, Credentials::bearer("bearer-token"));
    let error = uploader
        .upload_bytes(
            TEST_FILE.to_vec(),
            request()
                .ignore_warnings(true)
                .block_duplicates()
                .chunk_size(TEST_CHUNK_SIZE),
        )
        .await
        .expect_err("duplicate must be refused");

    match error {
        UploadError::Upload(failure) => {
            assert!(failure.to_string().contains("Other.jpg"));
            assert_eq!(failure.file_key.as_deref(), Some("stash-key"));
        }
        other => panic!("unexpected error: {other:?}"),
    }

    let bodies = bodies(server.received_requests().await.unwrap_or_default());
    assert!(
        !bodies
            .iter()
            .any(|body| body.contains("comment=") && body.contains("ignorewarnings=1"))
    );
}

#[tokio::test]
async fn chunked_upload_publishes_a_cosmetic_warning() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_chunks(&server).await;
    Mock::given(method("POST"))
        .and(path("/w/api.php"))
        .and(body_string_contains("comment="))
        .respond_with(|request: &wiremock::Request| {
            let body = String::from_utf8_lossy(&request.body);
            if body.contains("ignorewarnings=1") {
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "upload": {"result": "Success", "filename": "Example_image.jpg"}
                }))
            } else {
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "upload": {
                        "result": "Warning",
                        "warnings": {"was-deleted": "Example_image.jpg"},
                        "filekey": "stash-key"
                    }
                }))
            }
        })
        .expect(2)
        .mount(&server)
        .await;

    let uploader = uploader(&server, Credentials::bearer("bearer-token"));
    let result = uploader
        .upload_bytes(
            TEST_FILE.to_vec(),
            request()
                .ignore_warnings(true)
                .block_duplicates()
                .chunk_size(TEST_CHUNK_SIZE),
        )
        .await
        .expect("publish");

    assert_eq!(result.filename, "Example_image.jpg");
}

#[tokio::test]
async fn publishes_a_stashed_file_on_request() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("POST"))
        .and(path("/w/api.php"))
        .and(body_string_contains("filekey=stash-key"))
        .and(body_string_contains("ignorewarnings=1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "upload": {"result": "Success", "filename": "Example_image.jpg"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let uploader = uploader(&server, Credentials::bearer("bearer-token"));
    let result = uploader
        .publish_stashed(&request(), "stash-key")
        .await
        .expect("publish");

    assert_eq!(result.filename, "Example_image.jpg");
}

#[tokio::test]
async fn rejects_a_wrong_chunk_offset() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("POST"))
        .and(path("/w/api.php"))
        .and(body_string_contains("name=\"chunk\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "upload": {"result": "Continue", "offset": 999, "filekey": "stash-key"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let uploader = uploader(&server, Credentials::bearer("bearer-token"));
    let error = uploader
        .upload_bytes(TEST_FILE.to_vec(), request().chunk_size(TEST_CHUNK_SIZE))
        .await
        .expect_err("offset mismatch");

    assert!(matches!(
        error,
        UploadError::UnexpectedChunkOffset {
            expected: 5,
            actual: 999
        }
    ));
}

#[tokio::test]
async fn api_errors_are_returned_as_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/w/api.php"))
        .and(body_string_contains("meta=tokens"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "error": {"code": "badtoken", "info": "Invalid CSRF token"}
        })))
        .mount(&server)
        .await;

    let uploader = uploader(&server, Credentials::bearer("bearer-token"));
    let error = uploader
        .upload_bytes(b"test bytes".to_vec(), request())
        .await
        .expect_err("API error");

    match error {
        UploadError::Api(error) => {
            assert_eq!(error.code, "badtoken");
            assert_eq!(error.info.as_deref(), Some("Invalid CSRF token"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn rejects_non_http_api_urls() {
    let result = Uploader::new("file:///w/api.php", "test/1.0", Credentials::None);
    assert!(matches!(result, Err(UploadError::InvalidScheme)));
}
