use std::fmt;
use std::path::Path;

use gamepulse_application::{
    FencedSummaryWrite, GameReviewRefresh, GameReviewRefreshStore, ReviewExcerpt, ReviewInput,
    ReviewKind, ReviewPolarity, ReviewSummary, ReviewSummaryOutput, ReviewSummaryRequest,
    ReviewSummaryStore, SourceProductId,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::game_snapshot::upsert_snapshot_in_transaction;
use crate::job_queue::enqueue_derived_request;

/// SQLite implementation of both M011 review-refresh and fenced summary-store ports.
pub struct SqliteReviewSummaryStore {
    connection: Connection,
}

impl SqliteReviewSummaryStore {
    /// Open a file-backed database and apply all embedded storage migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ReviewSummaryStoreError> {
        let mut connection = Connection::open(path).map_err(ReviewSummaryStoreError::database)?;
        super::initialize_connection(&mut connection)
            .map_err(ReviewSummaryStoreError::migration)?;
        Ok(Self { connection })
    }

    /// Open an isolated in-memory database and apply all embedded storage migrations.
    pub fn open_in_memory() -> Result<Self, ReviewSummaryStoreError> {
        let mut connection =
            Connection::open_in_memory().map_err(ReviewSummaryStoreError::database)?;
        super::initialize_connection(&mut connection)
            .map_err(ReviewSummaryStoreError::migration)?;
        Ok(Self { connection })
    }
}

impl GameReviewRefreshStore for SqliteReviewSummaryStore {
    type Error = ReviewSummaryStoreError;

    fn persist_review_refresh(
        &mut self,
        refresh: &GameReviewRefresh,
    ) -> Result<(), ReviewSummaryStoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(ReviewSummaryStoreError::database)?;
        upsert_snapshot_in_transaction(&transaction, refresh.snapshot())
            .map_err(ReviewSummaryStoreError::snapshot)?;
        let product_id = sqlite_identifier(refresh.snapshot().source_product_id())?;
        if !matches_current_review_refresh(&transaction, product_id, refresh)? {
            for kind in ReviewKind::ALL {
                let input = refresh.input(kind);
                transaction
                    .execute(
                        "INSERT INTO review_inputs (
                            game_source_product_id, review_kind, content_hash, refresh_fingerprint
                         ) VALUES (?1, ?2, ?3, ?4)
                         ON CONFLICT(game_source_product_id, review_kind) DO UPDATE SET
                            content_hash = excluded.content_hash,
                            refresh_fingerprint = excluded.refresh_fingerprint",
                        params![
                            product_id,
                            kind.as_str(),
                            input.content_hash().as_str(),
                            refresh.fingerprint().as_str(),
                        ],
                    )
                    .map_err(ReviewSummaryStoreError::database)?;
                transaction
                    .execute(
                        "DELETE FROM review_input_excerpts
                         WHERE game_source_product_id = ?1 AND review_kind = ?2",
                        params![product_id, kind.as_str()],
                    )
                    .map_err(ReviewSummaryStoreError::database)?;
                for (position, excerpt) in input.excerpts().iter().enumerate() {
                    transaction
                        .execute(
                        "INSERT INTO review_input_excerpts (
                                game_source_product_id, review_kind, excerpt_position, excerpt, polarity
                             ) VALUES (?1, ?2, ?3, ?4, ?5)",
                            params![
                                product_id,
                                kind.as_str(),
                                i64::try_from(position).map_err(|_| {
                                    ReviewSummaryStoreError::malformed("review excerpt position")
                                })?,
                                excerpt.as_str(),
                                excerpt.polarity().map(ReviewPolarity::as_str),
                            ],
                        )
                        .map_err(ReviewSummaryStoreError::database)?;
                }
                transaction
                    .execute(
                        "INSERT INTO review_summaries (
                            game_source_product_id, review_kind, refresh_fingerprint, state
                         ) VALUES (?1, ?2, ?3, 'pending')
                         ON CONFLICT(game_source_product_id, review_kind) DO UPDATE SET
                            refresh_fingerprint = excluded.refresh_fingerprint,
                            state = 'pending'",
                        params![product_id, kind.as_str(), refresh.fingerprint().as_str()],
                    )
                    .map_err(ReviewSummaryStoreError::database)?;
                transaction
                    .execute(
                        "DELETE FROM review_summary_items
                         WHERE game_source_product_id = ?1 AND review_kind = ?2",
                        params![product_id, kind.as_str()],
                    )
                    .map_err(ReviewSummaryStoreError::database)?;
            }
            for request in refresh.jobs() {
                enqueue_derived_request(&transaction, request)
                    .map_err(ReviewSummaryStoreError::job)?;
            }
        }
        transaction
            .commit()
            .map_err(ReviewSummaryStoreError::database)
    }
}

fn matches_current_review_refresh(
    transaction: &rusqlite::Transaction<'_>,
    product_id: i64,
    refresh: &GameReviewRefresh,
) -> Result<bool, ReviewSummaryStoreError> {
    for kind in ReviewKind::ALL {
        let input = refresh.input(kind);
        let stored = transaction
            .query_row(
                "SELECT inputs.content_hash, inputs.refresh_fingerprint, summaries.refresh_fingerprint
                 FROM review_inputs AS inputs
                 INNER JOIN review_summaries AS summaries
                    ON summaries.game_source_product_id = inputs.game_source_product_id
                   AND summaries.review_kind = inputs.review_kind
                 WHERE inputs.game_source_product_id = ?1 AND inputs.review_kind = ?2",
                params![product_id, kind.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(ReviewSummaryStoreError::database)?;
        let Some((content_hash, input_fingerprint, summary_fingerprint)) = stored else {
            return Ok(false);
        };
        if content_hash != input.content_hash().as_str()
            || input_fingerprint != refresh.fingerprint().as_str()
            || summary_fingerprint != refresh.fingerprint().as_str()
        {
            return Ok(false);
        }
    }
    Ok(true)
}

impl ReviewSummaryStore for SqliteReviewSummaryStore {
    type Error = ReviewSummaryStoreError;

    fn load_review_input(
        &mut self,
        request: &ReviewSummaryRequest,
    ) -> Result<Option<ReviewInput>, ReviewSummaryStoreError> {
        let product_id = sqlite_identifier(request.source_product_id())?;
        let stored_hash = self
            .connection
            .query_row(
                "SELECT content_hash
                 FROM review_inputs
                 WHERE game_source_product_id = ?1
                   AND review_kind = ?2
                   AND refresh_fingerprint = ?3",
                params![
                    product_id,
                    request.kind().as_str(),
                    request.fingerprint().as_str()
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(ReviewSummaryStoreError::database)?;
        let Some(stored_hash) = stored_hash else {
            return Ok(None);
        };
        let mut statement = self
            .connection
            .prepare(
                "SELECT excerpt, polarity
                 FROM review_input_excerpts
                 WHERE game_source_product_id = ?1 AND review_kind = ?2
                 ORDER BY excerpt_position ASC",
            )
            .map_err(ReviewSummaryStoreError::database)?;
        let excerpts = statement
            .query_map(params![product_id, request.kind().as_str()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })
            .map_err(ReviewSummaryStoreError::database)?
            .map(|row| {
                row.map_err(ReviewSummaryStoreError::database)
                    .and_then(|(excerpt, polarity)| {
                        let polarity = match polarity.as_deref() {
                            None => None,
                            Some(value) => Some(ReviewPolarity::parse(value).ok_or_else(|| {
                                ReviewSummaryStoreError::malformed("stored review excerpt polarity")
                            })?),
                        };
                        ReviewExcerpt::with_polarity(excerpt, polarity).map_err(|_| {
                            ReviewSummaryStoreError::malformed("stored review excerpt")
                        })
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let input = ReviewInput::new(request.source_product_id(), request.kind(), excerpts)
            .map_err(|_| ReviewSummaryStoreError::malformed("stored review input"))?;
        if input.content_hash().as_str() != stored_hash {
            return Err(ReviewSummaryStoreError::malformed(
                "stored review input hash",
            ));
        }
        Ok(Some(input))
    }

    fn persist_review_summary(
        &mut self,
        summary: &ReviewSummary,
    ) -> Result<FencedSummaryWrite, ReviewSummaryStoreError> {
        let request = summary.request();
        let product_id = sqlite_identifier(request.source_product_id())?;
        let (state, items) = match summary.output() {
            ReviewSummaryOutput::Unavailable => ("unavailable", Vec::new()),
            ReviewSummaryOutput::Available { likes, dislikes } => {
                let likes = likes
                    .iter()
                    .enumerate()
                    .map(|(position, item)| ("like", position, item))
                    .collect::<Vec<_>>();
                let dislikes = dislikes
                    .iter()
                    .enumerate()
                    .map(|(position, item)| ("dislike", position, item))
                    .collect::<Vec<_>>();
                let mut items = likes;
                items.extend(dislikes);
                ("available", items)
            }
        };
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(ReviewSummaryStoreError::database)?;
        let changed = transaction
            .execute(
                "UPDATE review_summaries
                 SET state = ?1
                 WHERE game_source_product_id = ?2
                   AND review_kind = ?3
                   AND refresh_fingerprint = ?4",
                params![
                    state,
                    product_id,
                    request.kind().as_str(),
                    request.fingerprint().as_str(),
                ],
            )
            .map_err(ReviewSummaryStoreError::database)?;
        if changed == 0 {
            transaction
                .commit()
                .map_err(ReviewSummaryStoreError::database)?;
            return Ok(FencedSummaryWrite::Stale);
        }
        transaction
            .execute(
                "DELETE FROM review_summary_items
                 WHERE game_source_product_id = ?1 AND review_kind = ?2",
                params![product_id, request.kind().as_str()],
            )
            .map_err(ReviewSummaryStoreError::database)?;
        for (sentiment, position, item) in items {
            transaction
                .execute(
                    "INSERT INTO review_summary_items (
                        game_source_product_id, review_kind, sentiment, item_position, item
                     ) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        product_id,
                        request.kind().as_str(),
                        sentiment,
                        i64::try_from(position).map_err(|_| ReviewSummaryStoreError::malformed(
                            "summary item position"
                        ))?,
                        item,
                    ],
                )
                .map_err(ReviewSummaryStoreError::database)?;
        }
        transaction
            .commit()
            .map_err(ReviewSummaryStoreError::database)?;
        Ok(FencedSummaryWrite::Applied)
    }
}

fn sqlite_identifier(value: SourceProductId) -> Result<i64, ReviewSummaryStoreError> {
    i64::try_from(value.value())
        .map_err(|_| ReviewSummaryStoreError::malformed("game source product identity"))
}

/// Opaque adapter error surface for M011 review refreshes and summary fencing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewSummaryStoreError {
    message: String,
}

impl ReviewSummaryStoreError {
    fn database(error: rusqlite::Error) -> Self {
        Self {
            message: format!("SQLite review summary operation failed: {error}"),
        }
    }

    fn migration(_: super::DailyCrawlStateStoreError) -> Self {
        Self {
            message: "SQLite review summary migration failed".to_owned(),
        }
    }

    fn snapshot(_: crate::GameSnapshotStoreError) -> Self {
        Self {
            message: "SQLite game snapshot refresh failed".to_owned(),
        }
    }

    fn job(_: crate::JobStoreError) -> Self {
        Self {
            message: "SQLite review summary job refresh failed".to_owned(),
        }
    }

    fn malformed(field: &'static str) -> Self {
        Self {
            message: format!("SQLite review summary has invalid {field}"),
        }
    }
}

impl fmt::Display for ReviewSummaryStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ReviewSummaryStoreError {}

#[cfg(test)]
mod tests {
    use gamepulse_application::{
        GameReviewRefresh, GameReviewRefreshStore, GameSnapshot, JobTimestamp, ReviewExcerpt,
        ReviewInput, ReviewKind, ReviewSummaryJobSchedule, SourceProductId,
    };

    use super::*;

    fn input(kind: ReviewKind) -> ReviewInput {
        ReviewInput::new(
            SourceProductId::new(101).expect("test identity must be valid"),
            kind,
            vec![
                ReviewExcerpt::new("Synthetic review input.").expect("test excerpt must be valid"),
            ],
        )
        .expect("test review input must be valid")
    }

    #[test]
    fn failed_derived_summary_job_rolls_back_snapshot_and_both_review_inputs() {
        let mut store = SqliteReviewSummaryStore::open_in_memory().expect("store must open");
        store
            .connection
            .execute_batch(
                "CREATE TRIGGER fail_m011_job_insert
                 BEFORE INSERT ON jobs
                 WHEN NEW.job_type = 'llm.review-summary'
                 BEGIN
                     SELECT RAISE(ABORT, 'test summary job insert failure');
                 END;",
            )
            .expect("test trigger must install");
        let snapshot = GameSnapshot::new(
            SourceProductId::new(101).expect("test identity must be valid"),
            "example-game",
            "Example Game",
            "Synthetic description",
            None,
            None,
            Vec::new(),
            Vec::new(),
        )
        .expect("test snapshot must be valid");
        let refresh = GameReviewRefresh::new(
            snapshot,
            input(ReviewKind::Critic),
            input(ReviewKind::User),
            ReviewSummaryJobSchedule::new(1).expect("test schedule must be valid"),
            JobTimestamp::new(1).expect("test timestamp must be valid"),
        )
        .expect("test refresh must be valid");

        assert!(store.persist_review_refresh(&refresh).is_err());
        for table in [
            "games",
            "review_inputs",
            "review_input_excerpts",
            "review_summaries",
            "jobs",
        ] {
            let count = store
                .connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("rollback count must load");
            assert_eq!(count, 0, "{table} must remain empty after rollback");
        }
    }
}
