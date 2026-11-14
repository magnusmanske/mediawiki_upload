# mediawiki_upload

A small Rust library for uploading files to a MediaWiki wiki through its
Action API, with OAuth 1.0a (or OAuth 2 bearer) authentication.

It is the upload primitive shared by the Toolforge tools `url2commons`,
`flickr2commons`, `commonshelper` and `geograph2commons`. All four currently
funnel uploads through `magnustools/public_html/oauth_uploader.php` or
`Widar::upload_from_url()`, and then `MW_OAuth::doUploadFromURL()`; this crate
is the Rust replacement for that layer.

## What it does

* fetches the CSRF token (`action=query&meta=tokens`) and signs the request;
* uploads from a file path (streamed) or from memory (`action=upload`);
* supports chunked uploads for large files (`stash`, `offset`, `filesize`,
  `filekey`);
* normalizes file names the way the PHP tools did;
* implements MediaWiki's warning semantics:
  * warnings can be ignored (`ignorewarnings`),
  * or inspected first, with selected keys blocking the upload,
  * cosmetic warnings can then be overridden by publishing the stashed file
    (`filekey`), while e.g. `duplicate` still fails;
* returns the final file name and any warnings;
* can publish a stashed upload later via `publish_stashed()`.

## Chunked uploads

Chunking is opt-in, so a generic wiki does not have to support it. Enable it
for a request:

```rust
let request = UploadRequest::new(filename)
    .chunked()                 // 5 MiB chunks, files larger than that
    .ignore_warnings(true)
    .block_duplicates();

let result = uploader.upload_file("large-video.webm", request).await?;
```

`chunk_size(bytes)` chooses a different threshold and chunk size; a file no
larger than the threshold is sent in one request. `ignorewarnings` is set on
the chunks so the stash can be built, and warnings are checked when the
stashed file is committed at the end.

## Wikimedia feature

The default build is generic MediaWiki. The optional `wikimedia` feature adds
Wikimedia/Commons-specific helpers:

```toml
mediawiki_upload = { version = "0.1", features = ["wikimedia"] }
```

```rust
use mediawiki_upload::wikimedia::{bytes_facts, CommonsClient};

let facts = bytes_facts(&file_bytes);
let client = CommonsClient::new("my-tool/1.0 (https://tool.example; me@example.org)")?;
let check = client.check(&filename, &facts).await?;

if !check.is_free() {
    // page exists, or an identical current/deleted file is known
}
if check.needs_review() {
    // the name was deleted before, or the same file was deleted before
}
if !check.size_allowed() {
    // larger than the wiki's maxuploadsize
}
```

`file_facts(path)` computes the same facts for a file on disk. The checker
looks at the current file name, identical current files
(`list=allimages&aisha1`), identical deleted files
(`list=filearchive&fasha1`), the deletion log, and `maxuploadsize` from
`siteinfo`.

### Toolforge replicas

For high-volume checks on Toolforge, the `toolforge-replicas` feature replaces
the API round-trips with queries against the `commonswiki` replica:

```toml
mediawiki_upload = { version = "0.1", features = ["toolforge-replicas"] }
```

```rust
let check = mediawiki_upload::wikimedia::CommonsReplicas::new()?
    .check(&filename, &facts)
    .await?;
```

The database stores SHA-1 as base36, while the API uses hex; `Sha1Hash`
converts between them. The replica check returns the same `CommonsCheck`
shape, except that `max_upload_size` is `None` because it comes from site info.

**License note:** the `toolforge-replicas` feature enables the optional
[`toolforge`](https://crates.io/crates/toolforge) crate, which is
GPL-3.0-or-later. Builds with that feature are therefore subject to GPL
compatibility requirements. The default build remains MIT OR Apache-2.0.

## What it deliberately does not do

* **No OAuth login handshake.** The tool already has a login/session layer
  (`wikimisc`'s `oauth1` for the Rust tools, or its own web session); this
  crate takes an access token.
* **No URL downloading.** `url2commons` accepts arbitrary URLs; fetching them
  needs an SSRF guard and a size limit, which belong to the web-facing tool.
  Download with that layer and call `upload_file()`.
* **No automatic retries.** Uploads are not necessarily idempotent; retrying
  is a policy decision for the caller.

## Usage

```rust
use mediawiki_upload::{Credentials, FileName, UploadRequest, Uploader};

async fn upload() -> Result<(), mediawiki_upload::UploadError> {
    let uploader = Uploader::commons(
        "my-tool/1.0 (https://tool.example; me@example.org)",
        Credentials::oauth1("consumer key", "consumer secret", "access token", "access secret"),
    )?;

    let request = UploadRequest::new(FileName::new("Example photo.jpg")?)
        .text("== {{int:filedesc}} ==\n{{Information|Description=…}}\n")
        .comment("Transferred from https://example.org/photo")
        .ignore_warnings(true)
        .block_duplicates();

    let result = uploader.upload_file("photo.jpg", request).await?;
    println!("uploaded as {}", result.filename);
    Ok(())
}
```

For session-cookie authentication (for example after a bot-password login),
build a client with a cookie store and use `Uploader::with_client(...,
Credentials::None)`.

## Security notes

* OAuth secrets and bearer tokens are redacted in `Debug` output.
* The default HTTP client does not follow redirects, so an authenticated POST
  cannot be redirected somewhere else.
* `ignore_warnings` defaults to `false`; enabling it can overwrite an existing
  file. Use `block_duplicates()` when warnings are meant to be inspected.
* The crate contains no `unsafe` code.

## Testing

```sh
cargo test
cargo test --features wikimedia
cargo test --features toolforge-replicas   # compiles the replica code
```

The OAuth signature tests use vectors computed with the original PHP
`MW_OAuth::sign_request`; the upload and API tests run against a local
`wiremock` server. The replica queries need a live Toolforge environment and
are therefore compile-checked rather than exercised in CI.
