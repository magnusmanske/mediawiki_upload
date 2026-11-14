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
