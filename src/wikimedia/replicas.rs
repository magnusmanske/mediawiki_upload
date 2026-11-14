//! Commons checks against the Toolforge wiki replicas.
//!
//! This is the same information [`CommonsClient`](super::CommonsClient)
//! obtains through the API, read directly from the `commonswiki` replica. On
//! Toolforge it avoids several API round-trips, which matters when a tool is
//! checking many files.
//!
//! The `img_sha1` and `fa_sha1` database columns store base36, unlike the
//! hexadecimal hashes the API uses; [`Sha1Hash`](super::Sha1Hash) converts
//! between the two.
//!
//! The queries exclude suppressed entries (`fa_deleted`, `log_deleted`), so
//! they reveal no more than the public API. Other visibility rules are still
//! not applied; use the API when exact user-facing semantics matter.

use super::{CommonsCheck, FileFacts, FileInfo, Sha1Hash, WikimediaError};
use crate::filename::FileName;
use toolforge::pool::mysql_async::{Row, prelude::Queryable};
use toolforge::pool::{ANALYTICS_CLUSTER, WikiPool};

/// The database Commons lives in.
const COMMONSWIKI: &str = "commonswiki";
/// The replica tables these checks read.
const TABLES: &[&str] = &["page", "image", "logging", "filearchive"];
/// How many duplicate entries to return.
const MAX_RESULTS: usize = 50;

/// Commons checks through the Toolforge wiki replicas.
///
/// Constructing this loads the Toolforge credentials and builds a replica
/// pool, so it only works inside Toolforge (or with the `replica.my.cnf`
/// discovery the `toolforge` crate provides).
#[derive(Clone, Debug)]
pub struct CommonsReplicas {
    pool: WikiPool,
}

impl CommonsReplicas {
    /// Creates a replica pool on the analytics cluster.
    pub fn new() -> Result<Self, WikimediaError> {
        Ok(Self {
            pool: WikiPool::new(ANALYTICS_CLUSTER)?,
        })
    }

    /// Uses an existing replica pool, for callers that already share one.
    pub fn with_pool(pool: WikiPool) -> Self {
        Self { pool }
    }

    /// The underlying replica pool.
    pub fn pool(&self) -> &WikiPool {
        &self.pool
    }

    /// Runs the checks a transfer tool needs before uploading `filename`.
    pub async fn check(
        &self,
        filename: &FileName,
        facts: &FileFacts,
    ) -> Result<CommonsCheck, WikimediaError> {
        let mut conn = self.pool.connect_for_tables(COMMONSWIKI, TABLES).await?;
        let name = filename.as_str();
        let base36 = facts.sha1.to_base36();

        let page_rows: Vec<Row> = conn
            .exec(
                "SELECT page_title FROM page WHERE page_namespace=6 AND page_title=? LIMIT 1",
                (name,),
            )
            .await?;
        let page_exists = !page_rows.is_empty();

        let existing_rows: Vec<Row> = conn
            .exec(
                "SELECT img_name, img_size, img_sha1 FROM image WHERE img_name=? LIMIT 1",
                (name,),
            )
            .await?;
        let existing_file = existing_rows.first().map(file_info_from_row).transpose()?;

        let duplicate_sql = format!(
            "SELECT img_name, img_size, img_sha1 FROM image WHERE img_sha1=? LIMIT {MAX_RESULTS}"
        );
        let duplicate_rows: Vec<Row> = conn.exec(duplicate_sql, (base36.as_str(),)).await?;
        let duplicates = duplicate_rows
            .iter()
            .map(file_info_from_row)
            .collect::<Result<Vec<_>, _>>()?;

        let deleted_sql = format!(
            "SELECT fa_name, fa_size, fa_sha1 FROM filearchive \
             WHERE fa_sha1=? AND fa_deleted=0 LIMIT {MAX_RESULTS}"
        );
        let deleted_rows: Vec<Row> = conn.exec(deleted_sql, (base36.as_str(),)).await?;
        let deleted_duplicates = deleted_rows
            .iter()
            .map(file_info_from_row)
            .collect::<Result<Vec<_>, _>>()?;

        let log_rows: Vec<Row> = conn
            .exec(
                "SELECT 1 FROM logging WHERE log_type='delete' AND log_action='delete' \
                 AND log_namespace=6 AND log_title=? AND log_deleted=0 LIMIT 1",
                (name,),
            )
            .await?;
        let was_deleted = !log_rows.is_empty();

        Ok(CommonsCheck {
            page_exists,
            existing_file,
            duplicates,
            deleted_duplicates,
            was_deleted,
            max_upload_size: None,
            size: facts.size,
            sha1: facts.sha1,
        })
    }
}

fn file_info_from_row(row: &Row) -> Result<FileInfo, WikimediaError> {
    let name = row
        .get::<String, _>(0)
        .ok_or_else(|| unexpected("missing file name"))?;
    let size = row
        .get::<u64, _>(1)
        .ok_or_else(|| unexpected("missing file size"))?;
    let sha1 = row
        .get::<String, _>(2)
        .ok_or_else(|| unexpected("missing SHA-1"))?;
    Ok(FileInfo {
        name,
        size,
        sha1: Sha1Hash::from_base36(&sha1)?,
        mime: None,
        url: None,
    })
}

fn unexpected(message: &str) -> WikimediaError {
    WikimediaError::Unexpected {
        message: message.to_string(),
    }
}
