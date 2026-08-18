use std::fmt;
use std::path::Path;

use gamepulse_application::{
    BrowseCursor, CrawlDayKey, CrawlDiscoveryRequest, DiscoveryCandidate, DiscoveryPage,
    DurableRunDiscovery, DurableRunProgressOutcome, DurableRunProgressStore, GameReviewRefresh,
    JobClaimFence, JobRequest, JobTimestamp, RunSourceIngestionRequest, RuntimeJobType,
    SourceIngestionJobSchedule,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::job_queue::enqueue_derived_request;
use crate::review_summary::persist_review_refresh_in_transaction;

const MISSING_REQUIRED_VIDEO: &str = "missing_required_video";
const SOURCE_PROGRESS_MAX_ATTEMPTS: u32 = 3;
const MAX_BROWSE_PAGES: i64 = 8;

/// SQLite implementation of the durable mandatory exact-target run port.
pub struct SqliteRunProgressStore {
    connection: Connection,
}

impl SqliteRunProgressStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RunProgressStoreError> {
        let mut connection = Connection::open(path).map_err(RunProgressStoreError::database)?;
        super::initialize_connection(&mut connection)
            .map_err(|_| RunProgressStoreError::migration())?;
        Ok(Self { connection })
    }

    pub fn open_in_memory() -> Result<Self, RunProgressStoreError> {
        let mut connection =
            Connection::open_in_memory().map_err(RunProgressStoreError::database)?;
        super::initialize_connection(&mut connection)
            .map_err(|_| RunProgressStoreError::migration())?;
        Ok(Self { connection })
    }
}

impl DurableRunProgressStore for SqliteRunProgressStore {
    type Error = RunProgressStoreError;

    fn begin_or_resume(
        &mut self,
        day: &CrawlDayKey,
        target: usize,
        created_at: JobTimestamp,
        deadline_at: JobTimestamp,
        job_identity: &str,
        claim_fence: JobClaimFence,
        now: JobTimestamp,
    ) -> Result<Option<DurableRunDiscovery>, RunProgressStoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(RunProgressStoreError::database)?;
        if !has_current_claim(&transaction, job_identity, claim_fence, now)? {
            transaction
                .commit()
                .map_err(RunProgressStoreError::database)?;
            return Ok(None);
        }
        let run_id = format!("daily-{}", day.as_str());
        let target =
            i64::try_from(target).map_err(|_| RunProgressStoreError::malformed("target"))?;
        let existing = load_run(&transaction, &run_id)?;
        let run = if let Some(existing) = existing {
            if existing.target_count != target {
                return Err(RunProgressStoreError::malformed("run target"));
            }
            existing
        } else {
            transaction
                .execute(
                    "INSERT INTO runs (
                        run_id, day_key, target_count, accepted_count, state, source_phase,
                        browse_cursor, deadline_at, version, progress_fence, next_item_order,
                        browse_page_count, created_at, updated_at
                     ) VALUES (?1, ?2, ?3, 0, 'active', 'new_releases', NULL, ?4, 0, 0, 0, 0, ?5, ?5)",
                    params![
                        run_id,
                        day.as_str(),
                        target,
                        deadline_at.value(),
                        created_at.value(),
                    ],
                )
                .map_err(RunProgressStoreError::database)?;
            load_run(&transaction, &run_id)?
                .ok_or_else(|| RunProgressStoreError::malformed("new run"))?
        };
        if run.state != "active" {
            transaction
                .commit()
                .map_err(RunProgressStoreError::database)?;
            return Ok(None);
        }
        if now > run.deadline_at {
            fail_deadline(&transaction, &run, now)?;
            transaction
                .commit()
                .map_err(RunProgressStoreError::database)?;
            return Ok(None);
        }
        if run.progress_fence != 0 || has_scheduled_item(&transaction, &run.run_id)? {
            transaction
                .commit()
                .map_err(RunProgressStoreError::database)?;
            return Ok(None);
        }
        let Some(discovery) = discovery_for_run(&run)? else {
            fail_exhausted(&transaction, &run, now)?;
            transaction
                .commit()
                .map_err(RunProgressStoreError::database)?;
            return Ok(None);
        };
        transaction
            .commit()
            .map_err(RunProgressStoreError::database)?;
        Ok(Some(discovery))
    }

    fn load_progress_discovery(
        &mut self,
        run_id: &str,
        version: u64,
        job_identity: &str,
        claim_fence: JobClaimFence,
        now: JobTimestamp,
    ) -> Result<Option<DurableRunDiscovery>, RunProgressStoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(RunProgressStoreError::database)?;
        if !has_current_claim(&transaction, job_identity, claim_fence, now)? {
            transaction
                .commit()
                .map_err(RunProgressStoreError::database)?;
            return Ok(None);
        }
        let Some(run) = load_run(&transaction, run_id)? else {
            transaction
                .commit()
                .map_err(RunProgressStoreError::database)?;
            return Ok(None);
        };
        if run.state != "active"
            || run.version != version
            || run.progress_fence != version
            || has_scheduled_item(&transaction, &run.run_id)?
        {
            transaction
                .commit()
                .map_err(RunProgressStoreError::database)?;
            return Ok(None);
        }
        if now > run.deadline_at {
            fail_deadline(&transaction, &run, now)?;
            transaction
                .commit()
                .map_err(RunProgressStoreError::database)?;
            return Ok(None);
        }
        let discovery = discovery_for_run(&run)?;
        transaction
            .commit()
            .map_err(RunProgressStoreError::database)?;
        Ok(discovery)
    }

    fn record_discovery_page(
        &mut self,
        discovery: &DurableRunDiscovery,
        page: &DiscoveryPage,
        schedule: SourceIngestionJobSchedule,
        created_at: JobTimestamp,
        job_identity: &str,
        claim_fence: JobClaimFence,
        now: JobTimestamp,
    ) -> Result<DurableRunProgressOutcome, RunProgressStoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(RunProgressStoreError::database)?;
        if !has_current_claim(&transaction, job_identity, claim_fence, now)? {
            transaction
                .commit()
                .map_err(RunProgressStoreError::database)?;
            return Ok(DurableRunProgressOutcome::AlreadyTerminal);
        }
        let Some(mut run) = load_run(&transaction, discovery.run_id())? else {
            transaction
                .commit()
                .map_err(RunProgressStoreError::database)?;
            return Ok(DurableRunProgressOutcome::AlreadyTerminal);
        };
        if run.state != "active" || run.version != discovery.version() {
            transaction
                .commit()
                .map_err(RunProgressStoreError::database)?;
            return Ok(DurableRunProgressOutcome::AlreadyTerminal);
        }
        if now > run.deadline_at {
            fail_deadline(&transaction, &run, now)?;
            transaction
                .commit()
                .map_err(RunProgressStoreError::database)?;
            return Ok(DurableRunProgressOutcome::DeadlineExceeded);
        }
        match (discovery.request(), run.source_phase.as_str()) {
            (CrawlDiscoveryRequest::NewReleases, "new_releases") if run.progress_fence == 0 => {}
            (CrawlDiscoveryRequest::NewestBrowse { .. }, "browse")
                if run.progress_fence == discovery.version() => {}
            _ => {
                transaction
                    .commit()
                    .map_err(RunProgressStoreError::database)?;
                return Ok(DurableRunProgressOutcome::AlreadyTerminal);
            }
        }
        if matches!(
            discovery.request(),
            CrawlDiscoveryRequest::NewestBrowse { .. }
        ) && run.browse_page_count >= MAX_BROWSE_PAGES
        {
            fail_exhausted(&transaction, &run, now)?;
            transaction
                .commit()
                .map_err(RunProgressStoreError::database)?;
            return Ok(DurableRunProgressOutcome::SourceExhausted);
        }
        for (offset, candidate) in page.candidates().iter().enumerate() {
            let discovery_order = run
                .next_item_order
                .checked_add(
                    i64::try_from(offset)
                        .map_err(|_| RunProgressStoreError::malformed("discovery order"))?,
                )
                .ok_or_else(|| RunProgressStoreError::malformed("discovery order"))?;
            transaction
                .execute(
                    "INSERT INTO run_items (run_id, source_product_id, source_slug, discovery_order, state, job_identity, rejection_category)
                     VALUES (?1, ?2, ?3, ?4, 'pending', NULL, NULL)
                     ON CONFLICT(run_id, source_product_id) DO NOTHING",
                    params![
                        run.run_id,
                        candidate.source_product_id().value().to_string(),
                        candidate.source_slug(),
                        discovery_order,
                    ],
                )
                .map_err(RunProgressStoreError::database)?;
        }
        let (phase, cursor, browse_page_count) = match discovery.request() {
            CrawlDiscoveryRequest::NewReleases => ("browse", None, run.browse_page_count),
            CrawlDiscoveryRequest::NewestBrowse { .. } => match page.next_browse_cursor() {
                Some(cursor) => (
                    "browse",
                    Some(cursor.value().to_string()),
                    run.browse_page_count
                        .checked_add(1)
                        .ok_or_else(|| RunProgressStoreError::malformed("browse page count"))?,
                ),
                None => (
                    "exhausted",
                    None,
                    run.browse_page_count
                        .checked_add(1)
                        .ok_or_else(|| RunProgressStoreError::malformed("browse page count"))?,
                ),
            },
        };
        let version = increment_version(run.version)?;
        transaction
            .execute(
                "UPDATE runs
                 SET source_phase = ?1, browse_cursor = ?2, version = ?3, progress_fence = 0,
                     next_item_order = ?4, browse_page_count = ?5, updated_at = ?6
                 WHERE run_id = ?7 AND state = 'active' AND version = ?8",
                params![
                    phase,
                    cursor,
                    i64::try_from(version)
                        .map_err(|_| RunProgressStoreError::malformed("run version"))?,
                    run.next_item_order
                        .checked_add(
                            i64::try_from(page.candidates().len())
                                .map_err(|_| RunProgressStoreError::malformed("discovery order"))?
                        )
                        .ok_or_else(|| RunProgressStoreError::malformed("discovery order"))?,
                    browse_page_count,
                    now.value(),
                    run.run_id,
                    i64::try_from(run.version)
                        .map_err(|_| RunProgressStoreError::malformed("run version"))?,
                ],
            )
            .map_err(RunProgressStoreError::database)?;
        run = load_run(&transaction, &run.run_id)?
            .ok_or_else(|| RunProgressStoreError::malformed("run"))?;
        let outcome = schedule_next_or_progress(&transaction, &mut run, schedule, created_at, now)?;
        transaction
            .commit()
            .map_err(RunProgressStoreError::database)?;
        Ok(outcome)
    }

    fn persist_completed_item(
        &mut self,
        request: &RunSourceIngestionRequest,
        job_identity: &str,
        refresh: &GameReviewRefresh,
        schedule: SourceIngestionJobSchedule,
        created_at: JobTimestamp,
        claim_fence: JobClaimFence,
        now: JobTimestamp,
    ) -> Result<DurableRunProgressOutcome, RunProgressStoreError> {
        if refresh.snapshot().source_product_id().value()
            != request.source().source_product_id().value()
        {
            return Err(RunProgressStoreError::malformed("completed item identity"));
        }
        settle_item(
            &mut self.connection,
            request,
            job_identity,
            Some(refresh),
            schedule,
            created_at,
            claim_fence,
            now,
        )
    }

    fn reject_missing_required_video(
        &mut self,
        request: &RunSourceIngestionRequest,
        job_identity: &str,
        schedule: SourceIngestionJobSchedule,
        created_at: JobTimestamp,
        claim_fence: JobClaimFence,
        now: JobTimestamp,
    ) -> Result<DurableRunProgressOutcome, RunProgressStoreError> {
        settle_item(
            &mut self.connection,
            request,
            job_identity,
            None,
            schedule,
            created_at,
            claim_fence,
            now,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn settle_item(
    connection: &mut Connection,
    request: &RunSourceIngestionRequest,
    job_identity: &str,
    refresh: Option<&GameReviewRefresh>,
    schedule: SourceIngestionJobSchedule,
    created_at: JobTimestamp,
    claim_fence: JobClaimFence,
    now: JobTimestamp,
) -> Result<DurableRunProgressOutcome, RunProgressStoreError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(RunProgressStoreError::database)?;
    if !has_current_claim(&transaction, job_identity, claim_fence, now)? {
        transaction
            .commit()
            .map_err(RunProgressStoreError::database)?;
        return Ok(DurableRunProgressOutcome::AlreadyTerminal);
    }
    let Some(mut run) = load_run(&transaction, request.run_id())? else {
        transaction
            .commit()
            .map_err(RunProgressStoreError::database)?;
        return Ok(DurableRunProgressOutcome::AlreadyTerminal);
    };
    if run.state != "active" {
        transaction
            .commit()
            .map_err(RunProgressStoreError::database)?;
        return Ok(DurableRunProgressOutcome::AlreadyTerminal);
    }
    if now > run.deadline_at {
        fail_deadline(&transaction, &run, now)?;
        transaction
            .commit()
            .map_err(RunProgressStoreError::database)?;
        return Ok(DurableRunProgressOutcome::DeadlineExceeded);
    }
    let item = transaction
        .query_row(
            "SELECT state, job_identity FROM run_items
             WHERE run_id = ?1 AND source_product_id = ?2",
            params![
                request.run_id(),
                request.source().source_product_id().value().to_string()
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()
        .map_err(RunProgressStoreError::database)?;
    let Some((state, stored_job_identity)) = item else {
        transaction
            .commit()
            .map_err(RunProgressStoreError::database)?;
        return Ok(DurableRunProgressOutcome::AlreadyTerminal);
    };
    if state != "scheduled" || stored_job_identity.as_deref() != Some(job_identity) {
        transaction
            .commit()
            .map_err(RunProgressStoreError::database)?;
        return Ok(DurableRunProgressOutcome::AlreadyTerminal);
    }
    if let Some(refresh) = refresh {
        persist_review_refresh_in_transaction(&transaction, refresh)
            .map_err(|_| RunProgressStoreError::refresh())?;
        transaction
            .execute(
                "UPDATE run_items SET state = 'complete'
                 WHERE run_id = ?1 AND source_product_id = ?2 AND state = 'scheduled' AND job_identity = ?3",
                params![request.run_id(), request.source().source_product_id().value().to_string(), job_identity],
            )
            .map_err(RunProgressStoreError::database)?;
        transaction
            .execute(
                "UPDATE runs SET accepted_count = accepted_count + 1, version = version + 1, updated_at = ?1
                 WHERE run_id = ?2 AND state = 'active' AND accepted_count < target_count",
                params![now.value(), request.run_id()],
            )
            .map_err(RunProgressStoreError::database)?;
    } else {
        transaction
            .execute(
                "UPDATE run_items SET state = 'rejected', rejection_category = ?1
                 WHERE run_id = ?2 AND source_product_id = ?3 AND state = 'scheduled' AND job_identity = ?4",
                params![MISSING_REQUIRED_VIDEO, request.run_id(), request.source().source_product_id().value().to_string(), job_identity],
            )
            .map_err(RunProgressStoreError::database)?;
        transaction
            .execute(
                "UPDATE runs SET version = version + 1, updated_at = ?1 WHERE run_id = ?2 AND state = 'active'",
                params![now.value(), request.run_id()],
            )
            .map_err(RunProgressStoreError::database)?;
    }
    run = load_run(&transaction, request.run_id())?
        .ok_or_else(|| RunProgressStoreError::malformed("run"))?;
    let outcome = schedule_next_or_progress(&transaction, &mut run, schedule, created_at, now)?;
    transaction
        .commit()
        .map_err(RunProgressStoreError::database)?;
    Ok(outcome)
}

fn schedule_next_or_progress(
    transaction: &Transaction<'_>,
    run: &mut StoredRun,
    schedule: SourceIngestionJobSchedule,
    created_at: JobTimestamp,
    now: JobTimestamp,
) -> Result<DurableRunProgressOutcome, RunProgressStoreError> {
    if run.accepted_count == run.target_count {
        transaction
            .execute(
                "UPDATE runs SET state = 'succeeded', progress_fence = 0, updated_at = ?1
                 WHERE run_id = ?2 AND state = 'active' AND accepted_count = target_count",
                params![now.value(), run.run_id],
            )
            .map_err(RunProgressStoreError::database)?;
        return Ok(DurableRunProgressOutcome::Progressed);
    }
    if let Some(candidate) = next_pending_candidate(transaction, &run.run_id)? {
        let request = schedule
            .request_for_run(&run.run_id, &candidate, created_at)
            .map_err(|_| RunProgressStoreError::job())?;
        let changed = transaction
            .execute(
                "UPDATE run_items SET state = 'scheduled', job_identity = ?1
                 WHERE run_id = ?2 AND source_product_id = ?3 AND state = 'pending'",
                params![
                    request.identity(),
                    run.run_id,
                    candidate.source_product_id().value().to_string(),
                ],
            )
            .map_err(RunProgressStoreError::database)?;
        if changed != 1 {
            return Err(RunProgressStoreError::malformed("run item schedule fence"));
        }
        enqueue_derived_request(transaction, &request).map_err(|_| RunProgressStoreError::job())?;
        return Ok(DurableRunProgressOutcome::Progressed);
    }
    if run.source_phase == "exhausted" {
        fail_exhausted(transaction, run, now)?;
        return Ok(DurableRunProgressOutcome::SourceExhausted);
    }
    if run.source_phase == "browse" && run.browse_page_count >= MAX_BROWSE_PAGES {
        fail_exhausted(transaction, run, now)?;
        return Ok(DurableRunProgressOutcome::SourceExhausted);
    }
    let version = run.version;
    let discovery =
        discovery_for_run(run)?.ok_or_else(|| RunProgressStoreError::malformed("run phase"))?;
    let request = JobRequest::new(
        format!("source.run-progress:{}:{version}", run.run_id),
        RuntimeJobType::SourceHourlyDiscovery.as_str(),
        discovery.progress_work_reference(),
        SOURCE_PROGRESS_MAX_ATTEMPTS,
        created_at,
    )
    .map_err(|_| RunProgressStoreError::job())?;
    let changed = transaction
        .execute(
            "UPDATE runs SET progress_fence = ?1 WHERE run_id = ?2 AND state = 'active' AND progress_fence = 0 AND version = ?1",
            params![
                i64::try_from(version).map_err(|_| RunProgressStoreError::malformed("run version"))?,
                run.run_id,
            ],
        )
        .map_err(RunProgressStoreError::database)?;
    if changed != 1 {
        return Err(RunProgressStoreError::malformed("run progress fence"));
    }
    enqueue_derived_request(transaction, &request).map_err(|_| RunProgressStoreError::job())?;
    Ok(DurableRunProgressOutcome::Progressed)
}

fn next_pending_candidate(
    transaction: &Transaction<'_>,
    run_id: &str,
) -> Result<Option<DiscoveryCandidate>, RunProgressStoreError> {
    transaction
        .query_row(
            "SELECT source_product_id, source_slug FROM run_items
             WHERE run_id = ?1 AND state = 'pending'
             ORDER BY discovery_order ASC LIMIT 1",
            params![run_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(RunProgressStoreError::database)?
        .map(|(source_product_id, source_slug)| {
            let source_product_id = source_product_id
                .parse::<u64>()
                .map_err(|_| RunProgressStoreError::malformed("run item identity"))?;
            DiscoveryCandidate::new(source_product_id, source_slug)
                .map_err(|_| RunProgressStoreError::malformed("run item candidate"))
        })
        .transpose()
}

fn has_scheduled_item(
    transaction: &Transaction<'_>,
    run_id: &str,
) -> Result<bool, RunProgressStoreError> {
    transaction
        .query_row(
            "SELECT 1 FROM run_items WHERE run_id = ?1 AND state = 'scheduled' LIMIT 1",
            params![run_id],
            |_| Ok(()),
        )
        .optional()
        .map(|value| value.is_some())
        .map_err(RunProgressStoreError::database)
}

/// Validate the queue-owned active lease before any durable run state changes.
/// The same immediate transaction then validates the run deadline and applies the transition, so
/// a reclaimed worker cannot create pages, settle a candidate, or persist a game after its fence
/// has changed.
fn has_current_claim(
    transaction: &Transaction<'_>,
    job_identity: &str,
    claim_fence: JobClaimFence,
    now: JobTimestamp,
) -> Result<bool, RunProgressStoreError> {
    transaction
        .query_row(
            "SELECT 1
             FROM jobs
             WHERE job_identity = ?1
               AND state = 'claimed'
               AND claim_token = ?2
               AND lease_expires_at = ?3
               AND lease_expires_at > ?4",
            params![
                job_identity,
                i64::from(claim_fence.claim_token()),
                claim_fence.lease_expires_at().value(),
                now.value(),
            ],
            |_| Ok(()),
        )
        .optional()
        .map(|value| value.is_some())
        .map_err(RunProgressStoreError::database)
}

fn discovery_for_run(
    run: &StoredRun,
) -> Result<Option<DurableRunDiscovery>, RunProgressStoreError> {
    let request = match run.source_phase.as_str() {
        "new_releases" => CrawlDiscoveryRequest::NewReleases,
        "browse" => {
            let cursor = run
                .browse_cursor
                .as_deref()
                .map(|value| value.parse::<u64>().map(BrowseCursor::new))
                .transpose()
                .map_err(|_| RunProgressStoreError::malformed("browse cursor"))?;
            CrawlDiscoveryRequest::NewestBrowse { cursor }
        }
        "exhausted" => return Ok(None),
        _ => return Err(RunProgressStoreError::malformed("source phase")),
    };
    DurableRunDiscovery::new(run.run_id.clone(), request, run.version)
        .map(Some)
        .map_err(|_| RunProgressStoreError::malformed("run identifier"))
}

fn fail_deadline(
    transaction: &Transaction<'_>,
    run: &StoredRun,
    now: JobTimestamp,
) -> Result<(), RunProgressStoreError> {
    transaction
        .execute(
            "UPDATE runs SET state = 'failed_deadline', progress_fence = 0, updated_at = ?1
             WHERE run_id = ?2 AND state = 'active'",
            params![now.value(), run.run_id],
        )
        .map_err(RunProgressStoreError::database)?;
    Ok(())
}

fn fail_exhausted(
    transaction: &Transaction<'_>,
    run: &StoredRun,
    now: JobTimestamp,
) -> Result<(), RunProgressStoreError> {
    transaction
        .execute(
            "UPDATE runs SET state = 'failed_exhausted', progress_fence = 0, updated_at = ?1
             WHERE run_id = ?2 AND state = 'active'",
            params![now.value(), run.run_id],
        )
        .map_err(RunProgressStoreError::database)?;
    Ok(())
}

fn increment_version(version: u64) -> Result<u64, RunProgressStoreError> {
    version
        .checked_add(1)
        .ok_or_else(|| RunProgressStoreError::malformed("run version"))
}

#[derive(Clone, Debug)]
struct StoredRun {
    run_id: String,
    target_count: i64,
    accepted_count: i64,
    state: String,
    source_phase: String,
    browse_cursor: Option<String>,
    deadline_at: JobTimestamp,
    version: u64,
    progress_fence: u64,
    next_item_order: i64,
    browse_page_count: i64,
}

fn load_run(
    transaction: &Transaction<'_>,
    run_id: &str,
) -> Result<Option<StoredRun>, RunProgressStoreError> {
    transaction
        .query_row(
            "SELECT run_id, target_count, accepted_count, state, source_phase, browse_cursor,
                    deadline_at, version, progress_fence, next_item_order, browse_page_count
             FROM runs WHERE run_id = ?1",
            params![run_id],
            |row| {
                let deadline_at = row.get::<_, i64>(6)?;
                let version = row.get::<_, i64>(7)?;
                let progress_fence = row.get::<_, i64>(8)?;
                Ok(StoredRun {
                    run_id: row.get(0)?,
                    target_count: row.get(1)?,
                    accepted_count: row.get(2)?,
                    state: row.get(3)?,
                    source_phase: row.get(4)?,
                    browse_cursor: row.get(5)?,
                    deadline_at: JobTimestamp::new(deadline_at)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    version: u64::try_from(version).map_err(|_| rusqlite::Error::InvalidQuery)?,
                    progress_fence: u64::try_from(progress_fence)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    next_item_order: row.get(9)?,
                    browse_page_count: row.get(10)?,
                })
            },
        )
        .optional()
        .map_err(RunProgressStoreError::database)
}

/// Opaque run-progress storage failure. It does not carry source values or raw errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RunProgressStoreError;

impl RunProgressStoreError {
    fn database(_: rusqlite::Error) -> Self {
        Self
    }
    fn migration() -> Self {
        Self
    }
    fn malformed(_: &'static str) -> Self {
        Self
    }
    fn job() -> Self {
        Self
    }
    fn refresh() -> Self {
        Self
    }
}

impl fmt::Display for RunProgressStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SQLite durable run progress operation failed")
    }
}

impl std::error::Error for RunProgressStoreError {}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use gamepulse_application::{
        ClaimedJob, GameSnapshot, GameVideoLink, JobClaimFence, JobClaimRequest, JobCompletion,
        JobRequest, JobStore, ReviewInput, ReviewKind, ReviewSummaryJobSchedule, SourceProductId,
    };

    use super::*;

    static NEXT_DATABASE: AtomicUsize = AtomicUsize::new(0);

    struct TemporaryDatabase {
        path: PathBuf,
    }

    impl TemporaryDatabase {
        fn new(name: &str) -> Self {
            let sequence = NEXT_DATABASE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "gamepulse-m054-{name}-{}-{sequence}.sqlite3",
                std::process::id()
            ));
            let _ = fs::remove_file(&path);
            Self { path }
        }
    }

    impl Drop for TemporaryDatabase {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
            let _ = fs::remove_file(format!("{}-wal", self.path.display()));
            let _ = fs::remove_file(format!("{}-shm", self.path.display()));
        }
    }

    fn time(value: i64) -> JobTimestamp {
        JobTimestamp::new(value).expect("test timestamp must be valid")
    }

    fn day() -> CrawlDayKey {
        CrawlDayKey::new("2026-08-18").expect("test day must be valid")
    }

    fn candidate(id: u64) -> DiscoveryCandidate {
        DiscoveryCandidate::new(id, format!("game-{id}")).expect("test candidate must be valid")
    }

    fn refresh(id: u64) -> GameReviewRefresh {
        let source_product_id = SourceProductId::new(id).expect("test identity must be valid");
        let snapshot = GameSnapshot::new(
            source_product_id,
            format!("game-{id}"),
            "Fixture game",
            "Fixture description",
            None,
            Some(GameVideoLink::new("fixture-video").expect("test video must be valid")),
            Vec::new(),
            Vec::new(),
        )
        .expect("test snapshot must be valid");
        GameReviewRefresh::new(
            snapshot,
            ReviewInput::new(source_product_id, ReviewKind::Critic, Vec::new())
                .expect("test critic input must be valid"),
            ReviewInput::new(source_product_id, ReviewKind::User, Vec::new())
                .expect("test user input must be valid"),
            ReviewSummaryJobSchedule::new(1).expect("test summary schedule must be valid"),
            time(1),
        )
        .expect("test refresh must be valid")
    }

    fn scheduled(store: &SqliteRunProgressStore) -> (String, String, String) {
        store
            .connection
            .query_row(
                "SELECT run_id, source_product_id, job_identity FROM run_items
                 WHERE state = 'scheduled'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("one candidate must be scheduled")
    }

    fn enqueue_hourly(path: &Path, identity: &str, created_at: JobTimestamp) {
        let mut queue = crate::SqliteJobStore::open(path).expect("queue must open");
        queue
            .enqueue(
                JobRequest::new(
                    identity,
                    RuntimeJobType::SourceHourlyDiscovery.as_str(),
                    "hour-slot:0",
                    3,
                    created_at,
                )
                .expect("hourly request must be valid"),
            )
            .expect("hourly job must enqueue");
    }

    fn claim_source(path: &Path, at: JobTimestamp) -> ClaimedJob {
        claim_source_with_lease(path, at, 20)
    }

    fn claim_source_with_lease(path: &Path, at: JobTimestamp, lease_seconds: i64) -> ClaimedJob {
        let mut queue = crate::SqliteJobStore::open(path).expect("queue must open");
        queue
            .claim_next_matching(
                JobClaimRequest::new("run-progress-test", at, lease_seconds)
                    .expect("claim request must be valid"),
                &[
                    RuntimeJobType::SourceHourlyDiscovery,
                    RuntimeJobType::SourceGameIngestion,
                ],
            )
            .expect("source claim must query")
            .expect("source job must exist")
    }

    fn recover_source_lease(path: &Path, at: JobTimestamp) {
        let mut queue = crate::SqliteJobStore::open(path).expect("queue must open");
        assert!(
            queue
                .claim_next_matching(
                    JobClaimRequest::new("run-progress-recovery", at, 20)
                        .expect("recovery claim request must be valid"),
                    &[
                        RuntimeJobType::SourceHourlyDiscovery,
                        RuntimeJobType::SourceGameIngestion,
                    ],
                )
                .expect("recovery must query")
                .is_none(),
            "lease recovery must not also claim before retry backoff"
        );
    }

    fn complete_claim(path: &Path, claim: &ClaimedJob, at: JobTimestamp) {
        let mut queue = crate::SqliteJobStore::open(path).expect("queue must open");
        queue
            .complete(
                JobCompletion::new(claim.clone().into_claim(), at)
                    .expect("completion must be well-formed"),
            )
            .expect("current source claim must complete");
    }

    fn fence(claim: &ClaimedJob) -> JobClaimFence {
        JobClaimFence::from_claim(claim.claim())
    }

    fn run_version(store: &SqliteRunProgressStore) -> u64 {
        let value = store
            .connection
            .query_row("SELECT version FROM runs", [], |row| row.get::<_, i64>(0))
            .expect("run version must load");
        u64::try_from(value).expect("run version must be non-negative")
    }

    fn run_state(store: &SqliteRunProgressStore) -> (String, i64) {
        store
            .connection
            .query_row("SELECT state, accepted_count FROM runs", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .expect("run state must load")
    }

    #[test]
    fn missing_video_rejection_restarts_without_quota_and_reaches_exact_target() {
        let database = TemporaryDatabase::new("restart-exact-target");
        let schedule = SourceIngestionJobSchedule::new(1).expect("schedule must be valid");
        let mut store = SqliteRunProgressStore::open(&database.path).expect("store must open");
        enqueue_hourly(&database.path, "hourly-initial", time(1));
        let initial_claim = claim_source(&database.path, time(1));
        let initial = store
            .begin_or_resume(
                &day(),
                2,
                time(1),
                time(100),
                initial_claim.job().identity(),
                fence(&initial_claim),
                time(1),
            )
            .expect("run must start")
            .expect("new releases must be requested");
        assert_eq!(
            store
                .record_discovery_page(
                    &initial,
                    &DiscoveryPage::new(vec![candidate(1), candidate(2), candidate(10)], None),
                    schedule,
                    time(1),
                    initial_claim.job().identity(),
                    fence(&initial_claim),
                    time(1),
                )
                .expect("initial page must persist"),
            DurableRunProgressOutcome::Progressed
        );
        complete_claim(&database.path, &initial_claim, time(1));
        let (run_id, first_id, first_job) = scheduled(&store);
        assert_eq!(first_id, "1");
        let first = RunSourceIngestionRequest::new(&run_id, 1, "game-1")
            .expect("first request must be valid");
        let first_claim = claim_source(&database.path, time(2));
        assert_eq!(first_claim.job().identity(), first_job);
        assert_eq!(
            store
                .reject_missing_required_video(
                    &first,
                    &first_job,
                    schedule,
                    time(1),
                    fence(&first_claim),
                    time(2),
                )
                .expect("missing video must settle"),
            DurableRunProgressOutcome::Progressed
        );
        complete_claim(&database.path, &first_claim, time(2));
        assert_eq!(run_state(&store), ("active".to_owned(), 0));
        assert_eq!(scheduled(&store).1, "2");
        assert_eq!(
            store
                .reject_missing_required_video(
                    &first,
                    &first_job,
                    schedule,
                    time(1),
                    fence(&first_claim),
                    time(2),
                )
                .expect("stale rejection must be harmless"),
            DurableRunProgressOutcome::AlreadyTerminal
        );
        assert_eq!(scheduled(&store).1, "2");
        assert_eq!(
            store
                .connection
                .query_row("SELECT COUNT(*) FROM games", [], |row| row.get::<_, i64>(0))
                .expect("game count must load"),
            0
        );
        drop(store);

        let mut store = SqliteRunProgressStore::open(&database.path).expect("store must reopen");
        let (run_id, second_id, second_job) = scheduled(&store);
        assert_eq!(second_id, "2");
        let second = RunSourceIngestionRequest::new(&run_id, 2, "game-2")
            .expect("second request must be valid");
        let second_claim = claim_source(&database.path, time(3));
        assert_eq!(second_claim.job().identity(), second_job);
        store
            .reject_missing_required_video(
                &second,
                &second_job,
                schedule,
                time(1),
                fence(&second_claim),
                time(3),
            )
            .expect("restarted missing video must settle");
        complete_claim(&database.path, &second_claim, time(3));
        let (run_id, third_id, third_job) = scheduled(&store);
        assert_eq!(third_id, "10");
        let third = RunSourceIngestionRequest::new(&run_id, 10, "game-10")
            .expect("third request must be valid");
        let third_claim = claim_source(&database.path, time(4));
        assert_eq!(third_claim.job().identity(), third_job);
        store
            .persist_completed_item(
                &third,
                &third_job,
                &refresh(10),
                schedule,
                time(1),
                fence(&third_claim),
                time(4),
            )
            .expect("first complete item must persist");
        complete_claim(&database.path, &third_claim, time(4));
        assert_eq!(run_state(&store), ("active".to_owned(), 1));
        assert_eq!(
            store
                .persist_completed_item(
                    &third,
                    &third_job,
                    &refresh(10),
                    schedule,
                    time(1),
                    fence(&third_claim),
                    time(4)
                )
                .expect("stale complete item must be harmless"),
            DurableRunProgressOutcome::AlreadyTerminal
        );
        assert_eq!(run_state(&store), ("active".to_owned(), 1));

        let version = run_version(&store);
        let browse_claim = claim_source(&database.path, time(5));
        let browse = store
            .load_progress_discovery(
                &run_id,
                version,
                browse_claim.job().identity(),
                fence(&browse_claim),
                time(5),
            )
            .expect("browse discovery must load")
            .expect("later page must be requested");
        assert_eq!(
            browse.request(),
            CrawlDiscoveryRequest::NewestBrowse { cursor: None }
        );
        store
            .record_discovery_page(
                &browse,
                &DiscoveryPage::new(vec![candidate(4)], None),
                schedule,
                time(1),
                browse_claim.job().identity(),
                fence(&browse_claim),
                time(5),
            )
            .expect("later page must persist");
        complete_claim(&database.path, &browse_claim, time(5));
        let (_, fourth_id, fourth_job) = scheduled(&store);
        assert_eq!(fourth_id, "4");
        let fourth = RunSourceIngestionRequest::new(&run_id, 4, "game-4")
            .expect("fourth request must be valid");
        let fourth_claim = claim_source(&database.path, time(6));
        assert_eq!(fourth_claim.job().identity(), fourth_job);
        store
            .persist_completed_item(
                &fourth,
                &fourth_job,
                &refresh(4),
                schedule,
                time(1),
                fence(&fourth_claim),
                time(6),
            )
            .expect("target item must persist");
        complete_claim(&database.path, &fourth_claim, time(6));
        assert_eq!(run_state(&store), ("succeeded".to_owned(), 2));
        assert_eq!(
            store
                .connection
                .query_row("SELECT COUNT(*) FROM games", [], |row| row.get::<_, i64>(0))
                .expect("game count must load"),
            2
        );
    }

    #[test]
    fn source_exhaustion_and_deadline_are_terminal_without_success() {
        let database = TemporaryDatabase::new("terminal-runs");
        let schedule = SourceIngestionJobSchedule::new(1).expect("schedule must be valid");
        let mut store = SqliteRunProgressStore::open(&database.path).expect("store must open");
        enqueue_hourly(&database.path, "hourly-exhaustion", time(1));
        let initial_claim = claim_source(&database.path, time(1));
        let initial = store
            .begin_or_resume(
                &day(),
                2,
                time(1),
                time(100),
                initial_claim.job().identity(),
                fence(&initial_claim),
                time(1),
            )
            .expect("run must start")
            .expect("initial discovery must be available");
        store
            .record_discovery_page(
                &initial,
                &DiscoveryPage::new(vec![candidate(1)], None),
                schedule,
                time(1),
                initial_claim.job().identity(),
                fence(&initial_claim),
                time(1),
            )
            .expect("initial page must persist");
        complete_claim(&database.path, &initial_claim, time(1));
        let (run_id, _, job_identity) = scheduled(&store);
        let first =
            RunSourceIngestionRequest::new(&run_id, 1, "game-1").expect("request must be valid");
        let first_claim = claim_source(&database.path, time(2));
        assert_eq!(first_claim.job().identity(), job_identity);
        store
            .reject_missing_required_video(
                &first,
                &job_identity,
                schedule,
                time(1),
                fence(&first_claim),
                time(2),
            )
            .expect("missing video must settle");
        complete_claim(&database.path, &first_claim, time(2));
        let version = run_version(&store);
        let browse_claim = claim_source(&database.path, time(3));
        let browse = store
            .load_progress_discovery(
                &run_id,
                version,
                browse_claim.job().identity(),
                fence(&browse_claim),
                time(3),
            )
            .expect("browse must load")
            .expect("browse must be required");
        assert_eq!(
            store
                .record_discovery_page(
                    &browse,
                    &DiscoveryPage::new(Vec::new(), None),
                    schedule,
                    time(1),
                    browse_claim.job().identity(),
                    fence(&browse_claim),
                    time(3),
                )
                .expect("exhaustion must settle"),
            DurableRunProgressOutcome::SourceExhausted
        );
        complete_claim(&database.path, &browse_claim, time(3));
        assert_eq!(run_state(&store), ("failed_exhausted".to_owned(), 0));

        let next_day = CrawlDayKey::new("2026-08-19").expect("next day must be valid");
        enqueue_hourly(&database.path, "hourly-deadline", time(1));
        let deadline_claim = claim_source(&database.path, time(3));
        assert!(
            store
                .begin_or_resume(
                    &next_day,
                    2,
                    time(1),
                    time(2),
                    deadline_claim.job().identity(),
                    fence(&deadline_claim),
                    time(3),
                )
                .expect("deadline must be a terminal non-retry outcome")
                .is_none()
        );
        complete_claim(&database.path, &deadline_claim, time(3));
        let deadline_state = store
            .connection
            .query_row(
                "SELECT state FROM runs WHERE day_key = '2026-08-19'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("deadline state must persist");
        assert_eq!(deadline_state, "failed_deadline");
    }

    #[test]
    fn stale_reclaimed_claim_cannot_change_run_item_or_schedule_state() {
        let database = TemporaryDatabase::new("stale-reclaim");
        let schedule = SourceIngestionJobSchedule::new(3).expect("schedule must be valid");
        let mut first_store =
            SqliteRunProgressStore::open(&database.path).expect("first store must open");
        enqueue_hourly(&database.path, "hourly-stale", time(1));
        let hourly_claim = claim_source(&database.path, time(1));
        let initial = first_store
            .begin_or_resume(
                &day(),
                2,
                time(1),
                time(100),
                hourly_claim.job().identity(),
                fence(&hourly_claim),
                time(1),
            )
            .expect("run must start")
            .expect("initial discovery must exist");
        first_store
            .record_discovery_page(
                &initial,
                &DiscoveryPage::new(vec![candidate(1), candidate(2)], None),
                schedule,
                time(1),
                hourly_claim.job().identity(),
                fence(&hourly_claim),
                time(1),
            )
            .expect("initial page must persist");
        complete_claim(&database.path, &hourly_claim, time(1));

        let (run_id, first_id, first_job) = scheduled(&first_store);
        assert_eq!(first_id, "1");
        let first_request =
            RunSourceIngestionRequest::new(&run_id, 1, "game-1").expect("request must be valid");
        let stale_claim = claim_source(&database.path, time(2));
        assert_eq!(stale_claim.job().identity(), first_job);
        recover_source_lease(&database.path, time(22));
        let current_claim = claim_source(&database.path, time(52));
        assert_eq!(current_claim.job().identity(), first_job);
        assert_ne!(
            stale_claim.claim().claim_token(),
            current_claim.claim().claim_token(),
            "recovery must advance the queue-owned fence"
        );

        assert_eq!(
            first_store
                .persist_completed_item(
                    &first_request,
                    &first_job,
                    &refresh(1),
                    schedule,
                    time(1),
                    fence(&stale_claim),
                    time(52),
                )
                .expect("stale completion must be handled"),
            DurableRunProgressOutcome::AlreadyTerminal
        );
        assert_eq!(
            first_store
                .reject_missing_required_video(
                    &first_request,
                    &first_job,
                    schedule,
                    time(1),
                    fence(&stale_claim),
                    time(52),
                )
                .expect("stale settlement must be handled"),
            DurableRunProgressOutcome::AlreadyTerminal
        );
        assert_eq!(run_state(&first_store), ("active".to_owned(), 0));
        assert_eq!(scheduled(&first_store).1, "1");
        assert_eq!(
            first_store
                .connection
                .query_row("SELECT COUNT(*) FROM games", [], |row| row.get::<_, i64>(0))
                .expect("stale game count must load"),
            0,
            "a stale fence must not persist a game"
        );

        let mut second_store =
            SqliteRunProgressStore::open(&database.path).expect("second store must open");
        assert_eq!(
            second_store
                .reject_missing_required_video(
                    &first_request,
                    &first_job,
                    schedule,
                    time(1),
                    fence(&current_claim),
                    time(52),
                )
                .expect("current settlement must persist"),
            DurableRunProgressOutcome::Progressed
        );
        assert_eq!(scheduled(&second_store).1, "2");
        assert_eq!(
            second_store
                .connection
                .query_row(
                    "SELECT state FROM run_items WHERE source_product_id = '1'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .expect("first item state must load"),
            "rejected"
        );
    }

    #[test]
    fn post_deadline_item_settlement_fails_run_without_mutating_candidate_state() {
        let database = TemporaryDatabase::new("post-deadline");
        let schedule = SourceIngestionJobSchedule::new(3).expect("schedule must be valid");
        let mut store = SqliteRunProgressStore::open(&database.path).expect("store must open");
        enqueue_hourly(&database.path, "hourly-deadline-item", time(1));
        let hourly_claim = claim_source(&database.path, time(1));
        let initial = store
            .begin_or_resume(
                &day(),
                2,
                time(1),
                time(100),
                hourly_claim.job().identity(),
                fence(&hourly_claim),
                time(1),
            )
            .expect("run must start")
            .expect("initial discovery must exist");
        store
            .record_discovery_page(
                &initial,
                &DiscoveryPage::new(vec![candidate(1)], None),
                schedule,
                time(1),
                hourly_claim.job().identity(),
                fence(&hourly_claim),
                time(1),
            )
            .expect("initial page must persist");
        complete_claim(&database.path, &hourly_claim, time(1));

        let (run_id, _, item_job) = scheduled(&store);
        let item =
            RunSourceIngestionRequest::new(&run_id, 1, "game-1").expect("request must be valid");
        let item_claim = claim_source_with_lease(&database.path, time(2), 200);
        assert_eq!(item_claim.job().identity(), item_job);
        assert_eq!(
            store
                .reject_missing_required_video(
                    &item,
                    &item_job,
                    schedule,
                    time(1),
                    fence(&item_claim),
                    time(101),
                )
                .expect("deadline transition must persist"),
            DurableRunProgressOutcome::DeadlineExceeded
        );
        complete_claim(&database.path, &item_claim, time(101));
        assert_eq!(run_state(&store), ("failed_deadline".to_owned(), 0));
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT state FROM run_items WHERE source_product_id = '1'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .expect("candidate state must load"),
            "scheduled",
            "post-deadline work must not settle a candidate"
        );
        assert_eq!(
            store
                .connection
                .query_row("SELECT COUNT(*) FROM games", [], |row| row.get::<_, i64>(0))
                .expect("game count must load"),
            0
        );
    }

    #[test]
    fn browse_page_limit_is_eight_and_survives_restart_without_a_ninth_page() {
        let database = TemporaryDatabase::new("browse-page-limit");
        let schedule = SourceIngestionJobSchedule::new(3).expect("schedule must be valid");
        let mut store = SqliteRunProgressStore::open(&database.path).expect("store must open");
        enqueue_hourly(&database.path, "hourly-browse", time(1));
        let hourly_claim = claim_source(&database.path, time(1));
        let initial = store
            .begin_or_resume(
                &day(),
                2,
                time(1),
                time(100),
                hourly_claim.job().identity(),
                fence(&hourly_claim),
                time(1),
            )
            .expect("run must start")
            .expect("initial discovery must exist");
        store
            .record_discovery_page(
                &initial,
                &DiscoveryPage::new(Vec::new(), None),
                schedule,
                time(1),
                hourly_claim.job().identity(),
                fence(&hourly_claim),
                time(1),
            )
            .expect("new releases page must persist");
        complete_claim(&database.path, &hourly_claim, time(1));

        for page_number in 1..=MAX_BROWSE_PAGES {
            let at = time(page_number + 1);
            let progress_claim = claim_source(&database.path, at);
            let run_id = store
                .connection
                .query_row("SELECT run_id FROM runs", [], |row| row.get::<_, String>(0))
                .expect("run id must load");
            let discovery = store
                .load_progress_discovery(
                    &run_id,
                    run_version(&store),
                    progress_claim.job().identity(),
                    fence(&progress_claim),
                    at,
                )
                .expect("progress discovery must load")
                .expect("each of eight browse pages must be requested");
            assert!(matches!(
                discovery.request(),
                CrawlDiscoveryRequest::NewestBrowse { .. }
            ));
            let outcome = store
                .record_discovery_page(
                    &discovery,
                    &DiscoveryPage::new(
                        Vec::new(),
                        Some(BrowseCursor::new(
                            u64::try_from(page_number * 24).expect("cursor"),
                        )),
                    ),
                    schedule,
                    time(1),
                    progress_claim.job().identity(),
                    fence(&progress_claim),
                    at,
                )
                .expect("browse page must persist");
            let expected = if page_number == MAX_BROWSE_PAGES {
                DurableRunProgressOutcome::SourceExhausted
            } else {
                DurableRunProgressOutcome::Progressed
            };
            assert_eq!(outcome, expected);
            complete_claim(&database.path, &progress_claim, at);
            if page_number == 4 {
                drop(store);
                store = SqliteRunProgressStore::open(&database.path)
                    .expect("run store must reopen at durable browse boundary");
            }
        }
        assert_eq!(run_state(&store), ("failed_exhausted".to_owned(), 0));
        assert_eq!(
            store
                .connection
                .query_row("SELECT browse_page_count FROM runs", [], |row| row
                    .get::<_, i64>(0))
                .expect("browse page count must load"),
            MAX_BROWSE_PAGES
        );
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM jobs
                     WHERE job_type = 'source.hourly-discovery' AND state = 'ready'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("ready source job count must load"),
            0,
            "the ninth browse page must never be scheduled"
        );
    }

    #[test]
    fn migration_upgrade_reopen_and_clean_database_create_the_run_schema() {
        let database = TemporaryDatabase::new("migration-upgrade");
        let connection = Connection::open(&database.path).expect("legacy database must open");
        connection
            .execute_batch(crate::DAILY_CRAWL_MIGRATION_0001)
            .expect("v1 migration must apply");
        connection
            .execute_batch(crate::JOB_QUEUE_MIGRATION_0002)
            .expect("v2 migration must apply");
        connection
            .execute_batch(crate::GAME_SNAPSHOT_MIGRATION_0003)
            .expect("v3 migration must apply");
        connection
            .execute_batch(crate::REVIEW_SUMMARY_MIGRATION_0004)
            .expect("v4 migration must apply");
        connection
            .execute_batch(crate::PUBLIC_COVER_URL_MIGRATION_0005)
            .expect("v5 migration must apply");
        connection
            .execute_batch(crate::REVIEW_EXCERPT_POLARITY_MIGRATION_0006)
            .expect("v6 migration must apply");
        connection
            .execute_batch(crate::RETRY_BACKOFF_AND_SOURCE_PACING_MIGRATION_0007)
            .expect("v7 migration must apply");
        connection
            .pragma_update(
                None,
                "user_version",
                crate::RETRY_BACKOFF_AND_SOURCE_PACING_SCHEMA_VERSION,
            )
            .expect("legacy version must persist");
        drop(connection);

        let store = SqliteRunProgressStore::open(&database.path).expect("v7 database must upgrade");
        let version = store
            .connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .expect("schema version must load");
        assert_eq!(version, crate::SCHEMA_VERSION);
        drop(store);
        SqliteRunProgressStore::open(&database.path).expect("upgraded database must reopen");

        let clean =
            SqliteRunProgressStore::open_in_memory().expect("clean database must initialize");
        assert_eq!(
            clean
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('runs', 'run_items')",
                    [],
                    |row| { row.get::<_, i64>(0) }
                )
                .expect("run tables must load"),
            2
        );
    }
}
