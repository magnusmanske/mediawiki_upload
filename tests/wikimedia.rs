#![cfg(feature = "wikimedia")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use mediawiki_upload::FileName;
use mediawiki_upload::wikimedia::{CommonsClient, FileFacts, Sha1Hash, bytes_facts};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const HEX: &str = "a84d2a201166a7db1d3fdba63dece1b1d068e2f3";

fn client(server: &MockServer) -> CommonsClient {
    let http = reqwest::Client::builder()
        .user_agent("mediawiki_upload-test/1.0")
        .build()
        .expect("client");
    CommonsClient::with_client_and_url(http, format!("{}/w/api.php", server.uri()))
        .expect("CommonsClient")
}

#[tokio::test]
async fn checks_commons_through_the_api() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/w/api.php"))
        .and(query_param("titles", "File:Example.jpg"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "query": {
                "pages": [{
                    "title": "File:Example.jpg",
                    "imageinfo": [{
                        "size": 10,
                        "sha1": HEX,
                        "mime": "image/jpeg",
                        "url": "https://upload.wikimedia.org/example.jpg"
                    }]
                }]
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/w/api.php"))
        .and(query_param("list", "allimages"))
        .and(query_param("aisha1", HEX))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "query": {
                "allimages": [{
                    "name": "Other.jpg",
                    "size": 10,
                    "sha1": HEX,
                    "mime": "image/jpeg"
                }]
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/w/api.php"))
        .and(query_param("list", "filearchive"))
        .and(query_param("fasha1", HEX))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "query": {
                "filearchive": [{
                    "name": "Old.jpg",
                    "size": "10",
                    "sha1": HEX
                }]
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/w/api.php"))
        .and(query_param("list", "logevents"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "query": {"logevents": [{"title": "File:Example.jpg"}]}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/w/api.php"))
        .and(query_param("meta", "siteinfo"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "query": {
                "general": {"maxuploadsize": 1000, "minuploadchunksize": 1024}
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let filename = FileName::new("Example.jpg").expect("name");
    let facts = FileFacts {
        size: 10,
        sha1: Sha1Hash::from_hex(HEX).expect("sha1"),
    };
    let check = client(&server)
        .check(&filename, &facts)
        .await
        .expect("check");

    assert!(check.page_exists);
    assert_eq!(
        check.existing_file.as_ref().map(|file| file.name.as_str()),
        Some("Example.jpg")
    );
    assert_eq!(check.duplicates.len(), 1);
    assert_eq!(check.duplicates[0].name, "Other.jpg");
    assert_eq!(check.deleted_duplicates.len(), 1);
    assert_eq!(check.deleted_duplicates[0].name, "Old.jpg");
    assert!(check.was_deleted);
    assert_eq!(check.max_upload_size, Some(1000));
    assert!(check.same_as_existing());
    assert!(!check.is_free());
    assert!(check.needs_review());
    assert!(check.size_allowed());
}

#[tokio::test]
async fn reports_api_errors() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/w/api.php"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "error": {"code": "invalidsha1hash", "info": "The SHA1 hash provided is not valid."}
        })))
        .mount(&server)
        .await;

    let error = client(&server)
        .files_with_sha1(Sha1Hash::from_hex(HEX).expect("sha1"))
        .await
        .expect_err("API error");
    assert!(error.to_string().contains("invalidsha1hash"));
}

#[test]
fn bytes_facts_are_hex_sha1() {
    let facts = bytes_facts(b"test bytes");
    assert_eq!(facts.size, 10);
    assert_eq!(facts.sha1.to_hex().len(), 40);
}
