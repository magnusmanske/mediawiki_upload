//! File names in the form MediaWiki stores them.

use std::fmt;
use std::str::FromStr;
use thiserror::Error;
use url::Url;

/// MediaWiki's maximum file-name length, in bytes.
///
/// `UploadBase` restricts the name to 240 bytes because the `oi_archive_name`
/// database column is 255 bytes and old versions add a timestamp and `!`.
const MAX_FILE_NAME_BYTES: usize = 240;

/// Characters MediaWiki does not allow in a title.
const FORBIDDEN_CHARACTERS: [char; 8] = ['#', '<', '>', '[', ']', '|', '{', '}'];

/// A file name, normalized the way MediaWiki will store it.
///
/// A `FileName` is never empty, never carries a `File:` or `Image:` prefix,
/// uses underscores rather than spaces, and starts with an upper-case letter.
/// Constructing one therefore settles those questions before an upload.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct FileName(String);

/// Why a file name could not be constructed.
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum FileNameError {
    /// The name was empty, or consisted only of a namespace prefix.
    #[error("the file name is empty")]
    Empty,

    /// The name contains a character MediaWiki forbids in titles.
    #[error("the file name contains a forbidden character: {0:?}")]
    ForbiddenCharacter(char),

    /// The name is longer than MediaWiki's file-name limit.
    #[error("the file name is longer than {MAX_FILE_NAME_BYTES} bytes")]
    TooLong,

    /// A URL was given, but its path has no final segment to use as a name.
    #[error("the URL has no file name")]
    NoFileName,

    /// The URL could not be parsed.
    #[error("invalid URL: {0}")]
    Url(#[from] url::ParseError),
}

impl FileName {
    /// Normalizes a user- or API-supplied name.
    ///
    /// This is the normalization PHP tools did with
    /// `ucfirst(str_replace(' ', '_', $name))`, plus stripping a leading
    /// `File:`/`Image:` prefix, which callers sometimes include by mistake.
    pub fn new(name: &str) -> Result<Self, FileNameError> {
        let name = name.trim();
        let name = strip_prefix_ci(name, "File:")
            .or_else(|| strip_prefix_ci(name, "Image:"))
            .unwrap_or(name)
            .trim();
        if name.is_empty() {
            return Err(FileNameError::Empty);
        }
        if let Some(character) = name
            .chars()
            .find(|character| character.is_control() || FORBIDDEN_CHARACTERS.contains(character))
        {
            return Err(FileNameError::ForbiddenCharacter(character));
        }

        let name = name.replace(' ', "_");
        let mut characters = name.chars();
        let Some(first) = characters.next() else {
            return Err(FileNameError::Empty);
        };
        let mut normalized: String = first.to_uppercase().collect();
        normalized.push_str(characters.as_str());
        if normalized.len() > MAX_FILE_NAME_BYTES {
            return Err(FileNameError::TooLong);
        }
        Ok(Self(normalized))
    }

    /// Derives a file name from the last non-empty path segment of a URL.
    ///
    /// Query strings and fragments are ignored, and percent-encoding is
    /// decoded, so `https://example.org/a/My%20Photo.jpg?x=1` yields
    /// `My_Photo.jpg`.
    pub fn from_url(url: &str) -> Result<Self, FileNameError> {
        let url = Url::parse(url)?;
        let segment = url
            .path_segments()
            .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
            .ok_or(FileNameError::NoFileName)?;
        let decoded = urlencoding::decode_binary(segment.as_bytes());
        Self::new(&String::from_utf8_lossy(&decoded))
    }

    /// The normalized name.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The normalized name as an owned string.
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for FileName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for FileName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("FileName").field(&self.0).finish()
    }
}

impl AsRef<str> for FileName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl FromStr for FileName {
    type Err = FileNameError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::new(name)
    }
}

/// Case-insensitive prefix stripping that also stays on UTF-8 boundaries.
fn strip_prefix_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &text[prefix.len()..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_spaces_and_first_letter() {
        assert_eq!(
            FileName::new("  example image.jpg  ")
                .expect("name")
                .as_str(),
            "Example_image.jpg"
        );
    }

    #[test]
    fn strips_file_prefix() {
        assert_eq!(
            FileName::new("file:Example.jpg").expect("name").as_str(),
            "Example.jpg"
        );
        assert_eq!(
            FileName::new("IMAGE:Example.jpg").expect("name").as_str(),
            "Example.jpg"
        );
    }

    #[test]
    fn uppercases_non_ascii() {
        assert_eq!(FileName::new("ä.jpg").expect("name").as_str(), "Ä.jpg");
    }

    #[test]
    fn rejects_empty_names() {
        assert_eq!(FileName::new("   ").unwrap_err(), FileNameError::Empty);
        assert_eq!(FileName::new("File:").unwrap_err(), FileNameError::Empty);
    }

    #[test]
    fn rejects_forbidden_characters() {
        assert_eq!(
            FileName::new("bad#name.jpg").unwrap_err(),
            FileNameError::ForbiddenCharacter('#')
        );
        assert_eq!(
            FileName::new("line\nbreak.jpg").unwrap_err(),
            FileNameError::ForbiddenCharacter('\n')
        );
    }

    #[test]
    fn derives_names_from_urls() {
        assert_eq!(
            FileName::from_url("https://example.org/a/My%20Photo.jpg?x=1")
                .expect("name")
                .as_str(),
            "My_Photo.jpg"
        );
        assert_eq!(
            FileName::from_url("https://example.org/a/")
                .expect("name")
                .as_str(),
            "A"
        );
        assert_eq!(
            FileName::from_url("https://example.org/").unwrap_err(),
            FileNameError::NoFileName
        );
    }

    #[test]
    fn rejects_overlong_names() {
        let name = "a".repeat(MAX_FILE_NAME_BYTES + 1);
        assert_eq!(FileName::new(&name).unwrap_err(), FileNameError::TooLong);
        let name = "a".repeat(MAX_FILE_NAME_BYTES);
        assert!(FileName::new(&name).is_ok());
    }
}
