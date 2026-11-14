# mediawiki_upload

A small Rust library for uploading files to a MediaWiki wiki through its
Action API, with OAuth 1.0a (or OAuth 2 bearer) authentication.

It is the upload primitive shared by the Toolforge tools `url2commons`,
`flickr2commons`, `commonshelper` and `geograph2commons`. All four currently
funnel uploads through `magnustools/public_html/oauth_uploader.php` and
`MW_OAuth::doUploadFromURL()`; this crate is the Rust replacement for that
layer.

## What it does

* fetches the CSRF token (`action=query&meta=tokens`) and signs the request;
* uploads from a file path (streamed) or from memory (`action=upload`);
* normalizes file names the way the PHP tools did;
* implements MediaWiki's warning semantics:
  * warnings can be ignored (`ignorewarnings`),
  * or inspected first, with selected keys blocking the upload,
  * cosmetic warnings can then be overridden by publishing the stashed file
    (`filekey`), while e.g. `duplicate` still fails;
* returns the final file name and any warnings.

## What it deliberately does not do

* **No OAuth login handshake.** The tool already has a login/session layer
  (`wikimisc`'s `oauth1` for the Rust tools, or its own web session); this
  crate takes an access token.
* **No URL downloading.** `url2commons` accepts arbitrary URLs; fetching them
  needs an SSRF guard and a size limit, which belong to the web-facing tool.
  Download with that layer and call `upload_file()`.
* **No chunked uploads (yet).** A single multipart request matches the current
  PHP behaviour. Chunked uploads (`stash`/`offset`/`filesize`) would be a
  useful later addition for very large files.
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
```

The OAuth signature tests use vectors computed with the original PHP
`MW_OAuth::sign_request`; the upload tests run against a local `wiremock`
server.
