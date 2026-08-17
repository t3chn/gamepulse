use std::fmt;
use std::path::Path;

use gamepulse_application::{
    ClaimedJob, JOB_TEXT_MAX_BYTES, JobAttempt, JobAttemptOutcome, JobClaim, JobClaimRequest,
    JobCompletion, JobEnqueueResult, JobFailure, JobFailureResult, JobInputError, JobRecord,
    JobRequest, JobStatus, JobStore, JobTimestamp, RuntimeJobType, retry_not_before,
};
use rusqlite::{
    Connection, OptionalExtension, Transaction, TransactionBehavior, params, params_from_iter,
    types::Value,
};

const EXPIRED_LEASE_ERROR: &str = "lease expired";

type StoredJobRow = (
    String,
    String,
    String,
    i64,
    i64,
    String,
    i64,
    i64,
    Option<i64>,
    Option<String>,
    Option<i64>,
    i64,
    Option<i64>,
    Option<String>,
);

struct StoredJob {
    record: JobRecord,
    claim_token: u32,
}

/// A durable SQLite adapter for the application-owned job queue port.
pub struct SqliteJobStore {
    connection: Connection,
}

impl SqliteJobStore {
    /// Open a file-backed database and apply all embedded storage migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JobStoreError> {
        let mut connection = Connection::open(path).map_err(JobStoreError::database)?;
        super::initialize_connection(&mut connection).map_err(|_| JobStoreError::migration())?;
        Ok(Self { connection })
    }

    /// Open an isolated in-memory database and apply all embedded storage migrations.
    pub fn open_in_memory() -> Result<Self, JobStoreError> {
        let mut connection = Connection::open_in_memory().map_err(JobStoreError::database)?;
        super::initialize_connection(&mut connection).map_err(|_| JobStoreError::migration())?;
        Ok(Self { connection })
    }

    #[cfg(test)]
    fn install_attempt_finalize_failure_for_test(&self) {
        self.connection
            .execute_batch(
                "CREATE TRIGGER fail_job_attempt_finalization
                 BEFORE UPDATE OF outcome ON job_attempts
                 WHEN NEW.outcome IN ('retryable_failure', 'terminal_failure')
                 BEGIN
                     SELECT RAISE(ABORT, 'test job attempt finalization failure');
                 END;",
            )
            .expect("test trigger must install");
    }

    #[cfg(test)]
    fn install_attempt_insert_failure_for_test(&self) {
        self.connection
            .execute_batch(
                "CREATE TRIGGER fail_job_attempt_insert
                 BEFORE INSERT ON job_attempts
                 BEGIN
                     SELECT RAISE(ABORT, 'test job attempt insert failure');
                 END;",
            )
            .expect("test trigger must install");
    }
}

impl JobStore for SqliteJobStore {
    type Error = JobStoreError;

    fn enqueue(&mut self, request: JobRequest) -> Result<JobEnqueueResult, JobStoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(JobStoreError::database)?;
        let result = enqueue_request(&transaction, &request)?;
        transaction.commit().map_err(JobStoreError::database)?;
        Ok(result)
    }

    fn claim_next(
        &mut self,
        request: JobClaimRequest,
    ) -> Result<Option<ClaimedJob>, JobStoreError> {
        self.claim_next_matching(request, &[])
    }

    fn claim_next_matching(
        &mut self,
        request: JobClaimRequest,
        accepted_types: &[RuntimeJobType],
    ) -> Result<Option<ClaimedJob>, JobStoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(JobStoreError::database)?;
        recover_expired_claims(&transaction, request.claimed_at())?;

        let clock_regressed = if accepted_types.is_empty() {
            transaction
                .query_row(
                    "SELECT 1 FROM jobs
                     WHERE state = 'ready' AND updated_at > ?1
                     LIMIT 1",
                    params![request.claimed_at().value()],
                    |_| Ok(()),
                )
                .optional()
                .map_err(JobStoreError::database)?
                .is_some()
        } else {
            let placeholders = (2..=accepted_types.len() + 1)
                .map(|index| format!("?{index}"))
                .collect::<Vec<_>>()
                .join(", ");
            let selection = format!(
                "SELECT 1 FROM jobs
                 WHERE state = 'ready' AND updated_at > ?1
                   AND job_type IN ({placeholders})
                 LIMIT 1"
            );
            transaction
                .query_row(
                    &selection,
                    params_from_iter(
                        std::iter::once(Value::Integer(request.claimed_at().value())).chain(
                            accepted_types
                                .iter()
                                .map(|job_type| Value::Text(job_type.as_str().to_owned())),
                        ),
                    ),
                    |_| Ok(()),
                )
                .optional()
                .map_err(JobStoreError::database)?
                .is_some()
        };
        if clock_regressed {
            return Err(JobStoreError::clock_regression());
        }

        if let Some(pacing) = request.pacing() {
            let next_claim_at = transaction
                .query_row(
                    "SELECT next_claim_at FROM job_lane_pacing WHERE lane_key = ?1",
                    params![pacing.lane_key()],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(JobStoreError::database)?
                .map(parse_timestamp)
                .transpose()?;
            if matches!(next_claim_at, Some(next_claim_at) if next_claim_at > request.claimed_at())
            {
                transaction.commit().map_err(JobStoreError::database)?;
                return Ok(None);
            }
        }

        let job_identity = if accepted_types.is_empty() {
            transaction
                .query_row(
                    "SELECT job_identity
                     FROM jobs
                     WHERE state = 'ready' AND attempt_count < max_attempts
                       AND COALESCE(retry_not_before, updated_at) <= ?1
                     ORDER BY created_at, job_identity
                     LIMIT 1",
                    params![request.claimed_at().value()],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(JobStoreError::database)?
        } else {
            let placeholders = (2..=accepted_types.len() + 1)
                .map(|index| format!("?{index}"))
                .collect::<Vec<_>>()
                .join(", ");
            let selection = format!(
                "SELECT job_identity
                 FROM jobs
                 WHERE state = 'ready' AND attempt_count < max_attempts
                   AND COALESCE(retry_not_before, updated_at) <= ?1
                   AND job_type IN ({placeholders})
                 ORDER BY created_at, job_identity
                 LIMIT 1"
            );
            transaction
                .query_row(
                    &selection,
                    params_from_iter(
                        std::iter::once(Value::Integer(request.claimed_at().value())).chain(
                            accepted_types
                                .iter()
                                .map(|job_type| Value::Text(job_type.as_str().to_owned())),
                        ),
                    ),
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(JobStoreError::database)?
        };
        let Some(job_identity) = job_identity else {
            transaction.commit().map_err(JobStoreError::database)?;
            return Ok(None);
        };
        let before_claim =
            load_job(&transaction, &job_identity)?.ok_or_else(JobStoreError::missing_job)?;
        if before_claim.record.status() != JobStatus::Ready {
            return Err(JobStoreError::malformed(
                "ready job selection was not ready",
            ));
        }
        if request.claimed_at() < before_claim.record.updated_at() {
            return Err(JobStoreError::clock_regression());
        }
        let attempt_number = before_claim
            .record
            .attempt_count()
            .checked_add(1)
            .ok_or_else(|| JobStoreError::malformed("job attempt count overflows"))?;
        let claim_token = before_claim
            .claim_token
            .checked_add(1)
            .ok_or_else(|| JobStoreError::malformed("job claim token overflows"))?;
        let changed = transaction
            .execute(
                "UPDATE jobs
                 SET state = 'claimed',
                     attempt_count = ?1,
                     updated_at = ?2,
                     retry_not_before = NULL,
                     claimed_by = ?3,
                     lease_expires_at = ?4,
                     claim_token = ?5,
                     terminal_at = NULL,
                     last_error = NULL
                 WHERE job_identity = ?6
                   AND state = 'ready'
                   AND attempt_count = ?7
                   AND claim_token = ?8
                   AND claim_token = attempt_count
                   AND updated_at <= ?9
                   AND COALESCE(retry_not_before, updated_at) <= ?9",
                params![
                    i64::from(attempt_number),
                    request.claimed_at().value(),
                    request.worker_id(),
                    request.lease_expires_at().value(),
                    i64::from(claim_token),
                    &job_identity,
                    i64::from(before_claim.record.attempt_count()),
                    i64::from(before_claim.claim_token),
                    request.claimed_at().value(),
                ],
            )
            .map_err(JobStoreError::database)?;
        if changed != 1 {
            return Err(JobStoreError::stale_claim());
        }
        transaction
            .execute(
                "INSERT INTO job_attempts (
                    job_identity, attempt_number, claim_token, worker_id, started_at, finished_at,
                    outcome, error
                 ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, 'active', NULL)",
                params![
                    &job_identity,
                    i64::from(attempt_number),
                    i64::from(claim_token),
                    request.worker_id(),
                    request.claimed_at().value(),
                ],
            )
            .map_err(JobStoreError::database)?;
        if let Some(pacing) = request.pacing() {
            let next_claim_at = pacing
                .next_claim_at(request.claimed_at())
                .map_err(JobStoreError::invalid_input)?;
            transaction
                .execute(
                    "INSERT INTO job_lane_pacing (lane_key, next_claim_at)
                     VALUES (?1, ?2)
                     ON CONFLICT(lane_key) DO UPDATE SET next_claim_at = excluded.next_claim_at",
                    params![pacing.lane_key(), next_claim_at.value()],
                )
                .map_err(JobStoreError::database)?;
        }
        let claimed =
            load_job(&transaction, &job_identity)?.ok_or_else(JobStoreError::missing_job)?;
        let claim = claim_from_stored_job(&claimed)?;
        let result = ClaimedJob::restored(claimed.record, claim);
        transaction.commit().map_err(JobStoreError::database)?;
        Ok(Some(result))
    }

    fn complete(&mut self, completion: JobCompletion) -> Result<(), JobStoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(JobStoreError::database)?;
        let stored = load_job(&transaction, completion.claim().identity())?
            .ok_or_else(JobStoreError::missing_job)?;
        let persisted_claim = claim_from_stored_job(&stored)?;
        if completion.claim() != &persisted_claim {
            return Err(JobStoreError::stale_claim());
        }
        let changed = transaction
            .execute(
                "UPDATE jobs
                 SET state = 'succeeded',
                     updated_at = ?1,
                     retry_not_before = NULL,
                     claimed_by = NULL,
                     lease_expires_at = NULL,
                     terminal_at = ?1,
                     last_error = NULL
                 WHERE job_identity = ?2
                   AND state = 'claimed'
                   AND claim_token = ?3
                   AND claimed_by = ?4
                   AND lease_expires_at > ?1",
                params![
                    completion.completed_at().value(),
                    completion.claim().identity(),
                    i64::from(completion.claim().claim_token()),
                    completion.claim().worker_id(),
                ],
            )
            .map_err(JobStoreError::database)?;
        if changed != 1 {
            return Err(JobStoreError::stale_claim());
        }
        finish_attempt(
            &transaction,
            completion.claim(),
            completion.completed_at(),
            "succeeded",
            None,
        )?;
        transaction.commit().map_err(JobStoreError::database)
    }

    fn next_claim_eligible_at(
        &mut self,
        request: JobClaimRequest,
        accepted_types: &[RuntimeJobType],
    ) -> Result<Option<JobTimestamp>, JobStoreError> {
        let ready_at = earliest_job_timestamp(
            &self.connection,
            "ready",
            "COALESCE(retry_not_before, updated_at)",
            accepted_types,
        )?;
        let ready_at = match (ready_at, request.pacing()) {
            (Some(ready_at), Some(pacing)) => {
                let lane_next_claim_at = self
                    .connection
                    .query_row(
                        "SELECT next_claim_at FROM job_lane_pacing WHERE lane_key = ?1",
                        params![pacing.lane_key()],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()
                    .map_err(JobStoreError::database)?
                    .map(parse_timestamp)
                    .transpose()?;
                Some(lane_next_claim_at.map_or(ready_at, |lane_at| ready_at.max(lane_at)))
            }
            (ready_at, None) => ready_at,
            (None, Some(_)) => None,
        };
        let lease_expiry_at = earliest_job_timestamp(
            &self.connection,
            "claimed",
            "lease_expires_at",
            accepted_types,
        )?;
        Ok(match (ready_at, lease_expiry_at) {
            (Some(ready_at), Some(lease_expiry_at)) => Some(ready_at.min(lease_expiry_at)),
            (Some(ready_at), None) => Some(ready_at),
            (None, Some(lease_expiry_at)) => Some(lease_expiry_at),
            (None, None) => None,
        })
    }

    fn fail(&mut self, failure: JobFailure) -> Result<JobFailureResult, JobStoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(JobStoreError::database)?;
        let stored = load_job(&transaction, failure.claim().identity())?
            .ok_or_else(JobStoreError::missing_job)?;
        let terminal = stored.record.attempt_count() >= stored.record.max_attempts();
        let state = if terminal { "failed" } else { "ready" };
        let outcome = if terminal {
            "terminal_failure"
        } else {
            "retryable_failure"
        };
        let terminal_at = terminal.then_some(failure.failed_at().value());
        let retry_not_before = if terminal {
            None
        } else {
            Some(
                retry_not_before(failure.failed_at(), stored.record.attempt_count())
                    .map_err(JobStoreError::invalid_input)?
                    .value(),
            )
        };
        let changed = transaction
            .execute(
                "UPDATE jobs
                 SET state = ?1,
                     updated_at = ?2,
                     retry_not_before = ?3,
                     claimed_by = NULL,
                     lease_expires_at = NULL,
                     terminal_at = ?4,
                     last_error = ?5
                 WHERE job_identity = ?6
                   AND state = 'claimed'
                   AND claim_token = ?7
                   AND claimed_by = ?8
                   AND lease_expires_at > ?2",
                params![
                    state,
                    failure.failed_at().value(),
                    retry_not_before,
                    terminal_at,
                    failure.error(),
                    failure.claim().identity(),
                    i64::from(failure.claim().claim_token()),
                    failure.claim().worker_id(),
                ],
            )
            .map_err(JobStoreError::database)?;
        if changed != 1 {
            return Err(JobStoreError::stale_claim());
        }
        finish_attempt(
            &transaction,
            failure.claim(),
            failure.failed_at(),
            outcome,
            Some(failure.error()),
        )?;
        transaction.commit().map_err(JobStoreError::database)?;
        Ok(if terminal {
            JobFailureResult::Failed
        } else {
            JobFailureResult::ReadyForRetry
        })
    }

    fn job(&mut self, identity: &str) -> Result<Option<JobRecord>, JobStoreError> {
        validate_lookup_identity(identity)?;
        load_job(&self.connection, identity).map(|job| job.map(|stored| stored.record))
    }

    fn attempts(&mut self, identity: &str) -> Result<Vec<JobAttempt>, JobStoreError> {
        validate_lookup_identity(identity)?;
        if load_job(&self.connection, identity)?.is_none() {
            return Ok(Vec::new());
        }
        load_attempts(&self.connection, identity)
    }
}

fn earliest_job_timestamp(
    connection: &Connection,
    state: &str,
    timestamp_expression: &str,
    accepted_types: &[RuntimeJobType],
) -> Result<Option<JobTimestamp>, JobStoreError> {
    let type_clause = if accepted_types.is_empty() {
        String::new()
    } else {
        let placeholders = (2..=accepted_types.len() + 1)
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(" AND job_type IN ({placeholders})")
    };
    let attempt_clause = if state == "ready" {
        " AND attempt_count < max_attempts"
    } else {
        ""
    };
    let query = format!(
        "SELECT MIN({timestamp_expression})
         FROM jobs
         WHERE state = ?1{attempt_clause}{type_clause}"
    );
    connection
        .query_row(
            &query,
            params_from_iter(
                std::iter::once(Value::Text(state.to_owned())).chain(
                    accepted_types
                        .iter()
                        .map(|job_type| Value::Text(job_type.as_str().to_owned())),
                ),
            ),
            |row| row.get::<_, Option<i64>>(0),
        )
        .map_err(JobStoreError::database)?
        .map(parse_timestamp)
        .transpose()
}

/// Insert one request through an existing transaction so another durable state transition can
/// share the queue's identity/deduplication semantics without creating a nested transaction.
pub(crate) fn enqueue_request(
    transaction: &Transaction<'_>,
    request: &JobRequest,
) -> Result<JobEnqueueResult, JobStoreError> {
    enqueue_request_with_duplicate_validation(transaction, request, false)
}

/// Insert a job derived by an atomic daily-crawl commit.
///
/// Unlike the general queue port, this boundary may only accept a replay when the durable job
/// still exactly matches its deterministic derived request. This prevents a mutable candidate
/// slug from advancing without changing the already-derived ingestion work reference.
pub(crate) fn enqueue_derived_request(
    transaction: &Transaction<'_>,
    request: &JobRequest,
) -> Result<JobEnqueueResult, JobStoreError> {
    enqueue_request_with_duplicate_validation(transaction, request, true)
}

fn enqueue_request_with_duplicate_validation(
    transaction: &Transaction<'_>,
    request: &JobRequest,
    require_exact_duplicate: bool,
) -> Result<JobEnqueueResult, JobStoreError> {
    let inserted = transaction
        .execute(
            "INSERT INTO jobs (
                job_identity, job_type, work_ref, max_attempts, attempt_count, state,
                created_at, updated_at, retry_not_before, claimed_by, lease_expires_at,
                claim_token, terminal_at, last_error
             ) VALUES (?1, ?2, ?3, ?4, 0, 'ready', ?5, ?5, ?5, NULL, NULL, 0, NULL, NULL)
             ON CONFLICT(job_identity) DO NOTHING",
            params![
                request.identity(),
                request.job_type(),
                request.work_ref(),
                i64::from(request.max_attempts()),
                request.created_at().value(),
            ],
        )
        .map_err(JobStoreError::database)?;
    let stored =
        load_job(transaction, request.identity())?.ok_or_else(JobStoreError::missing_job)?;

    if inserted == 1 {
        return Ok(JobEnqueueResult::Enqueued(stored.record));
    }

    if require_exact_duplicate
        && (stored.record.job_type() != request.job_type()
            || stored.record.work_ref() != request.work_ref()
            || stored.record.max_attempts() != request.max_attempts())
    {
        return Err(JobStoreError::duplicate_request_conflict());
    }

    Ok(JobEnqueueResult::Duplicate(stored.record))
}

/// A non-leaking error surface for SQLite queue operations and malformed durable state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobStoreError {
    message: &'static str,
}

impl JobStoreError {
    fn database(_error: rusqlite::Error) -> Self {
        Self {
            message: "SQLite job queue operation failed",
        }
    }

    fn migration() -> Self {
        Self {
            message: "SQLite job queue migration or schema validation failed",
        }
    }

    fn malformed(_detail: &'static str) -> Self {
        Self {
            message: "malformed persisted job queue state",
        }
    }

    fn duplicate_request_conflict() -> Self {
        Self {
            message: "job identity conflicts with a different durable request",
        }
    }

    fn stale_claim() -> Self {
        Self {
            message: "job claim is stale, expired, or no longer active",
        }
    }

    fn clock_regression() -> Self {
        Self {
            message: "job queue clock regressed before durable state",
        }
    }

    fn missing_job() -> Self {
        Self {
            message: "job disappeared during queue operation",
        }
    }

    fn invalid_input(_error: JobInputError) -> Self {
        Self {
            message: "invalid job queue input",
        }
    }
}

impl fmt::Display for JobStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for JobStoreError {}

fn recover_expired_claims(
    transaction: &Transaction<'_>,
    recovered_at: JobTimestamp,
) -> Result<(), JobStoreError> {
    let expired = {
        let mut statement = transaction
            .prepare(
                "SELECT job_identity
                 FROM jobs
                 WHERE state = 'claimed' AND lease_expires_at <= ?1
                 ORDER BY job_identity",
            )
            .map_err(JobStoreError::database)?;
        statement
            .query_map(params![recovered_at.value()], |row| row.get::<_, String>(0))
            .map_err(JobStoreError::database)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(JobStoreError::database)?
    };

    for identity in expired {
        let stored = load_job(transaction, &identity)?.ok_or_else(JobStoreError::missing_job)?;
        if stored.record.status() != JobStatus::Claimed {
            return Err(JobStoreError::malformed("expired job was not claimed"));
        }
        let terminal = stored.record.attempt_count() >= stored.record.max_attempts();
        let state = if terminal { "failed" } else { "ready" };
        let terminal_at = terminal.then_some(recovered_at.value());
        let retry_not_before = if terminal {
            None
        } else {
            Some(
                retry_not_before(recovered_at, stored.record.attempt_count())
                    .map_err(JobStoreError::invalid_input)?
                    .value(),
            )
        };
        let changed = transaction
            .execute(
                "UPDATE jobs
                 SET state = ?1,
                     updated_at = ?2,
                     retry_not_before = ?3,
                     claimed_by = NULL,
                     lease_expires_at = NULL,
                     terminal_at = ?4,
                     last_error = ?5
                 WHERE job_identity = ?6
                   AND state = 'claimed'
                   AND claim_token = ?7
                   AND lease_expires_at <= ?2",
                params![
                    state,
                    recovered_at.value(),
                    retry_not_before,
                    terminal_at,
                    EXPIRED_LEASE_ERROR,
                    &identity,
                    i64::from(stored.claim_token),
                ],
            )
            .map_err(JobStoreError::database)?;
        if changed != 1 {
            return Err(JobStoreError::stale_claim());
        }
        let changed = transaction
            .execute(
                "UPDATE job_attempts
                 SET finished_at = ?1, outcome = 'expired', error = NULL
                 WHERE job_identity = ?2 AND claim_token = ?3 AND outcome = 'active'",
                params![
                    recovered_at.value(),
                    &identity,
                    i64::from(stored.claim_token),
                ],
            )
            .map_err(JobStoreError::database)?;
        if changed != 1 {
            return Err(JobStoreError::malformed(
                "expired job has no active attempt",
            ));
        }
    }
    Ok(())
}

fn claim_from_stored_job(stored: &StoredJob) -> Result<JobClaim, JobStoreError> {
    if stored.record.status() != JobStatus::Claimed {
        return Err(JobStoreError::malformed("claimed result was not claimed"));
    }
    let worker_id = stored
        .record
        .claimed_by()
        .ok_or_else(|| JobStoreError::malformed("claimed job has no owner"))?
        .to_owned();
    let lease_expires_at = stored
        .record
        .lease_expires_at()
        .ok_or_else(|| JobStoreError::malformed("claimed job has no lease"))?;
    Ok(JobClaim::restored(
        stored.record.identity().to_owned(),
        worker_id,
        stored.claim_token,
        stored.record.updated_at(),
        lease_expires_at,
    ))
}

fn finish_attempt(
    transaction: &Transaction<'_>,
    claim: &JobClaim,
    finished_at: JobTimestamp,
    outcome: &str,
    error: Option<&str>,
) -> Result<(), JobStoreError> {
    let changed = transaction
        .execute(
            "UPDATE job_attempts
             SET finished_at = ?1, outcome = ?2, error = ?3
             WHERE job_identity = ?4 AND claim_token = ?5 AND outcome = 'active'",
            params![
                finished_at.value(),
                outcome,
                error,
                claim.identity(),
                i64::from(claim.claim_token()),
            ],
        )
        .map_err(JobStoreError::database)?;
    if changed != 1 {
        return Err(JobStoreError::malformed("active job attempt is missing"));
    }
    Ok(())
}

fn load_job(connection: &Connection, identity: &str) -> Result<Option<StoredJob>, JobStoreError> {
    let row = connection
        .query_row(
            "SELECT job_identity, job_type, work_ref, max_attempts, attempt_count, state,
                    created_at, updated_at, retry_not_before, claimed_by, lease_expires_at,
                    claim_token, terminal_at, last_error
             FROM jobs
             WHERE job_identity = ?1",
            params![identity],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<i64>>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, Option<i64>>(12)?,
                    row.get::<_, Option<String>>(13)?,
                ))
            },
        )
        .optional()
        .map_err(JobStoreError::database)?;
    let stored = row.map(decode_job).transpose()?;
    if let Some(stored) = &stored {
        verify_attempt_history(connection, stored)?;
    }
    Ok(stored)
}

fn load_attempts(
    connection: &Connection,
    identity: &str,
) -> Result<Vec<JobAttempt>, JobStoreError> {
    let mut statement = connection
        .prepare(
            "SELECT attempt_number, claim_token, worker_id, started_at, finished_at, outcome, error
             FROM job_attempts
             WHERE job_identity = ?1
             ORDER BY claim_token",
        )
        .map_err(JobStoreError::database)?;
    let rows = statement
        .query_map(params![identity], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })
        .map_err(JobStoreError::database)?;
    rows.map(|row| {
        let (attempt_number, claim_token, worker_id, started_at, finished_at, outcome, error) =
            row.map_err(JobStoreError::database)?;
        decode_attempt(
            attempt_number,
            claim_token,
            worker_id,
            started_at,
            finished_at,
            outcome,
            error,
        )
    })
    .collect()
}

fn verify_attempt_history(
    connection: &Connection,
    stored: &StoredJob,
) -> Result<(), JobStoreError> {
    let (count, min_attempt_number, max_attempt_number, min_claim_token, max_claim_token, pairs) =
        connection
            .query_row(
                "SELECT
                    COUNT(*),
                    MIN(attempt_number),
                    MAX(attempt_number),
                    MIN(claim_token),
                    MAX(claim_token),
                    COALESCE(SUM(CASE WHEN attempt_number = claim_token THEN 0 ELSE 1 END), 0)
                 FROM job_attempts
                 WHERE job_identity = ?1",
                params![stored.record.identity()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .map_err(JobStoreError::database)?;
    let expected = i64::from(stored.record.attempt_count());
    let continuous = if expected == 0 {
        count == 0
    } else {
        count == expected
            && min_attempt_number == Some(1)
            && max_attempt_number == Some(expected)
            && min_claim_token == Some(1)
            && max_claim_token == Some(expected)
            && pairs == 0
    };
    if !continuous {
        return Err(JobStoreError::malformed(
            "job counters and attempt history are not fenced consistently",
        ));
    }
    Ok(())
}

fn decode_job(row: StoredJobRow) -> Result<StoredJob, JobStoreError> {
    let (
        identity,
        job_type,
        work_ref,
        max_attempts,
        attempt_count,
        raw_status,
        created_at,
        updated_at,
        retry_not_before,
        claimed_by,
        lease_expires_at,
        claim_token,
        terminal_at,
        last_error,
    ) = row;
    validate_stored_text(&identity)?;
    validate_stored_text(&job_type)?;
    validate_stored_text(&work_ref)?;
    let max_attempts = parse_u32(max_attempts)?;
    if max_attempts == 0 {
        return Err(JobStoreError::malformed("job maximum attempts is zero"));
    }
    let attempt_count = parse_u32(attempt_count)?;
    let claim_token = parse_u32(claim_token)?;
    if attempt_count > max_attempts || claim_token != attempt_count {
        return Err(JobStoreError::malformed("job counts are inconsistent"));
    }
    let status = parse_job_status(&raw_status)?;
    let created_at = parse_timestamp(created_at)?;
    let updated_at = parse_timestamp(updated_at)?;
    if created_at > updated_at {
        return Err(JobStoreError::malformed("job timestamps regress"));
    }
    let retry_not_before = retry_not_before.map(parse_timestamp).transpose()?;
    let lease_expires_at = lease_expires_at.map(parse_timestamp).transpose()?;
    let terminal_at = terminal_at.map(parse_timestamp).transpose()?;
    if let Some(worker_id) = &claimed_by {
        validate_stored_text(worker_id)?;
    }
    if let Some(error) = &last_error {
        validate_stored_text(error)?;
    }
    match status {
        JobStatus::Ready => {
            if retry_not_before.is_none()
                || claimed_by.is_some()
                || lease_expires_at.is_some()
                || terminal_at.is_some()
            {
                return Err(JobStoreError::malformed(
                    "ready job has claim or terminal fields",
                ));
            }
        }
        JobStatus::Claimed => {
            if retry_not_before.is_some()
                || claimed_by.is_none()
                || lease_expires_at.is_none()
                || terminal_at.is_some()
                || lease_expires_at <= Some(updated_at)
            {
                return Err(JobStoreError::malformed(
                    "claimed job fields are inconsistent",
                ));
            }
        }
        JobStatus::Succeeded => {
            if retry_not_before.is_some()
                || claimed_by.is_some()
                || lease_expires_at.is_some()
                || terminal_at < Some(updated_at)
                || last_error.is_some()
            {
                return Err(JobStoreError::malformed(
                    "succeeded job fields are inconsistent",
                ));
            }
        }
        JobStatus::Failed => {
            if retry_not_before.is_some()
                || claimed_by.is_some()
                || lease_expires_at.is_some()
                || terminal_at < Some(updated_at)
                || last_error.is_none()
            {
                return Err(JobStoreError::malformed(
                    "failed job fields are inconsistent",
                ));
            }
        }
    }
    Ok(StoredJob {
        record: JobRecord::restored(
            identity,
            job_type,
            work_ref,
            max_attempts,
            attempt_count,
            status,
            created_at,
            updated_at,
            retry_not_before,
            claimed_by,
            lease_expires_at,
            terminal_at,
            last_error,
        ),
        claim_token,
    })
}

fn decode_attempt(
    attempt_number: i64,
    claim_token: i64,
    worker_id: String,
    started_at: i64,
    finished_at: Option<i64>,
    raw_outcome: String,
    error: Option<String>,
) -> Result<JobAttempt, JobStoreError> {
    validate_stored_text(&worker_id)?;
    if let Some(error) = &error {
        validate_stored_text(error)?;
    }
    let attempt_number = parse_u32(attempt_number)?;
    let claim_token = parse_u32(claim_token)?;
    if attempt_number == 0 || claim_token == 0 {
        return Err(JobStoreError::malformed("job attempt number is zero"));
    }
    let started_at = parse_timestamp(started_at)?;
    let finished_at = finished_at.map(parse_timestamp).transpose()?;
    if matches!(finished_at, Some(finished_at) if finished_at < started_at) {
        return Err(JobStoreError::malformed(
            "job attempt finished before it started",
        ));
    }
    let outcome = parse_attempt_outcome(&raw_outcome)?;
    match outcome {
        JobAttemptOutcome::Active if finished_at.is_none() && error.is_none() => {}
        JobAttemptOutcome::Succeeded | JobAttemptOutcome::Expired
            if finished_at.is_some() && error.is_none() => {}
        JobAttemptOutcome::RetryableFailure | JobAttemptOutcome::TerminalFailure
            if finished_at.is_some() && error.is_some() => {}
        _ => {
            return Err(JobStoreError::malformed(
                "job attempt fields are inconsistent",
            ));
        }
    }
    Ok(JobAttempt::restored(
        attempt_number,
        claim_token,
        worker_id,
        started_at,
        finished_at,
        outcome,
        error,
    ))
}

fn parse_job_status(value: &str) -> Result<JobStatus, JobStoreError> {
    match value {
        "ready" => Ok(JobStatus::Ready),
        "claimed" => Ok(JobStatus::Claimed),
        "succeeded" => Ok(JobStatus::Succeeded),
        "failed" => Ok(JobStatus::Failed),
        _ => Err(JobStoreError::malformed("unknown job status")),
    }
}

fn parse_attempt_outcome(value: &str) -> Result<JobAttemptOutcome, JobStoreError> {
    match value {
        "active" => Ok(JobAttemptOutcome::Active),
        "succeeded" => Ok(JobAttemptOutcome::Succeeded),
        "retryable_failure" => Ok(JobAttemptOutcome::RetryableFailure),
        "terminal_failure" => Ok(JobAttemptOutcome::TerminalFailure),
        "expired" => Ok(JobAttemptOutcome::Expired),
        _ => Err(JobStoreError::malformed("unknown job attempt outcome")),
    }
}

fn parse_u32(value: i64) -> Result<u32, JobStoreError> {
    u32::try_from(value).map_err(|_| JobStoreError::malformed("job integer is out of range"))
}

fn parse_timestamp(value: i64) -> Result<JobTimestamp, JobStoreError> {
    JobTimestamp::new(value).map_err(JobStoreError::invalid_input)
}

fn validate_lookup_identity(identity: &str) -> Result<(), JobStoreError> {
    validate_stored_text(identity)
        .map_err(|_| JobStoreError::invalid_input(JobInputError::BlankText("job identity")))
}

fn validate_stored_text(value: &str) -> Result<(), JobStoreError> {
    if value.trim().is_empty() || value.len() > JOB_TEXT_MAX_BYTES {
        return Err(JobStoreError::malformed("stored text is invalid"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEMPORARY_DATABASE: AtomicU64 = AtomicU64::new(0);

    struct TemporaryDatabase {
        path: PathBuf,
    }

    impl TemporaryDatabase {
        fn new(name: &str) -> Self {
            let sequence = NEXT_TEMPORARY_DATABASE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "gamepulse-job-queue-{name}-{}-{sequence}.sqlite3",
                process::id()
            ));
            let _ = fs::remove_file(&path);
            Self { path }
        }

        fn open(&self) -> SqliteJobStore {
            SqliteJobStore::open(&self.path).expect("test queue must open")
        }
    }

    impl Drop for TemporaryDatabase {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    fn time(value: i64) -> JobTimestamp {
        JobTimestamp::new(value).expect("test timestamp must be valid")
    }

    fn job(identity: &str, max_attempts: u32, created_at: i64) -> JobRequest {
        JobRequest::new(
            identity,
            "source.fetch",
            "game:101",
            max_attempts,
            time(created_at),
        )
        .expect("test job request must be valid")
    }

    fn claim(store: &mut SqliteJobStore, worker: &str, now: i64, lease: i64) -> ClaimedJob {
        store
            .claim_next(
                JobClaimRequest::new(worker, time(now), lease)
                    .expect("test claim request must be valid"),
            )
            .expect("claim must succeed")
            .expect("job must be available")
    }

    #[test]
    fn enqueue_deduplicates_identity_without_overwriting_the_first_job() {
        let mut store = SqliteJobStore::open_in_memory().expect("queue must open");
        let first = store
            .enqueue(job("game:101", 3, 10))
            .expect("first enqueue must succeed");
        let duplicate = store
            .enqueue(
                JobRequest::new("game:101", "other.type", "other:work", 9, time(99))
                    .expect("duplicate request must be valid"),
            )
            .expect("duplicate enqueue must succeed");

        assert!(matches!(first, JobEnqueueResult::Enqueued(_)));
        let JobEnqueueResult::Duplicate(record) = duplicate else {
            panic!("second enqueue must deduplicate");
        };
        assert_eq!(record.job_type(), "source.fetch");
        assert_eq!(record.work_ref(), "game:101");
        assert_eq!(record.max_attempts(), 3);
        assert_eq!(record.created_at(), time(10));
    }

    #[test]
    fn malformed_counter_and_token_gap_is_rejected_before_a_claim_can_reuse_a_token() {
        let mut store = SqliteJobStore::open_in_memory().expect("queue must open");
        store
            .connection
            .execute(
                "INSERT INTO jobs (
                    job_identity, job_type, work_ref, max_attempts, attempt_count, state,
                    created_at, updated_at, claimed_by, lease_expires_at, claim_token, terminal_at,
                    last_error
                 ) VALUES (?1, 'source.fetch', 'game:101', 3, 2, 'ready', 10, 10, NULL, NULL, 0, NULL, NULL)",
                params!["game:101"],
            )
            .expect("malformed test row must insert");

        assert!(
            store
                .claim_next(
                    JobClaimRequest::new("worker", time(10), 5).expect("claim must be valid")
                )
                .is_err()
        );
        let stored = store
            .connection
            .query_row(
                "SELECT state, attempt_count, claim_token FROM jobs WHERE job_identity = ?1",
                params!["game:101"],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .expect("malformed row must remain unchanged");
        assert_eq!(stored, ("ready".to_owned(), 2, 0));
        assert!(
            store
                .complete(
                    JobCompletion::new(
                        JobClaim::restored(
                            "game:101".to_owned(),
                            "worker".to_owned(),
                            1,
                            time(10),
                            time(15)
                        ),
                        time(11),
                    )
                    .expect("stale completion must be well-formed"),
                )
                .is_err()
        );
    }

    #[test]
    fn complete_rejects_malformed_claimed_history_without_mutating_durable_state() {
        let mut store = SqliteJobStore::open_in_memory().expect("queue must open");
        store
            .connection
            .execute(
                "INSERT INTO jobs (
                    job_identity, job_type, work_ref, max_attempts, attempt_count, state,
                    created_at, updated_at, claimed_by, lease_expires_at, claim_token, terminal_at,
                    last_error
                 ) VALUES (?1, 'source.fetch', 'game:102', 3, 2, 'claimed', 10, 10, 'worker', 20, 1, NULL, NULL)",
                params!["game:102"],
            )
            .expect("malformed claimed test job must insert");
        store
            .connection
            .execute(
                "INSERT INTO job_attempts (
                    job_identity, attempt_number, claim_token, worker_id, started_at, finished_at,
                    outcome, error
                 ) VALUES (?1, 2, 1, 'worker', 10, NULL, 'active', NULL)",
                params!["game:102"],
            )
            .expect("malformed claimed test attempt must insert");
        let before_job = store
            .connection
            .query_row(
                "SELECT state, attempt_count, claim_token, claimed_by, lease_expires_at, updated_at
                 FROM jobs WHERE job_identity = ?1",
                params!["game:102"],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .expect("malformed claimed job must load");
        let before_attempt = store
            .connection
            .query_row(
                "SELECT attempt_number, claim_token, worker_id, started_at, finished_at, outcome
                 FROM job_attempts WHERE job_identity = ?1",
                params!["game:102"],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .expect("malformed claimed attempt must load");

        assert!(
            store
                .complete(
                    JobCompletion::new(
                        JobClaim::restored(
                            "game:102".to_owned(),
                            "worker".to_owned(),
                            1,
                            time(10),
                            time(20),
                        ),
                        time(11),
                    )
                    .expect("stale completion must be well-formed"),
                )
                .is_err()
        );
        let after_job = store
            .connection
            .query_row(
                "SELECT state, attempt_count, claim_token, claimed_by, lease_expires_at, updated_at
                 FROM jobs WHERE job_identity = ?1",
                params!["game:102"],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .expect("malformed claimed job must remain");
        let after_attempt = store
            .connection
            .query_row(
                "SELECT attempt_number, claim_token, worker_id, started_at, finished_at, outcome
                 FROM job_attempts WHERE job_identity = ?1",
                params!["game:102"],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .expect("malformed claimed attempt must remain");
        assert_eq!(after_job, before_job);
        assert_eq!(after_attempt, before_attempt);
    }

    #[test]
    fn malformed_attempt_history_with_mismatched_fencing_pairs_is_rejected_before_claim() {
        let mut store = SqliteJobStore::open_in_memory().expect("queue must open");
        store
            .connection
            .execute(
                "INSERT INTO jobs (
                    job_identity, job_type, work_ref, max_attempts, attempt_count, state,
                    created_at, updated_at, claimed_by, lease_expires_at, claim_token, terminal_at,
                    last_error
                 ) VALUES (?1, 'source.fetch', 'game:102', 3, 2, 'ready', 10, 10, NULL, NULL, 2, NULL, NULL)",
                params!["game:102"],
            )
            .expect("malformed test job must insert");
        for (attempt_number, claim_token) in [(1_i64, 2_i64), (2_i64, 1_i64)] {
            store
                .connection
                .execute(
                    "INSERT INTO job_attempts (
                        job_identity, attempt_number, claim_token, worker_id, started_at, finished_at,
                        outcome, error
                     ) VALUES (?1, ?2, ?3, 'worker', 10, 10, 'expired', NULL)",
                    params!["game:102", attempt_number, claim_token],
                )
                .expect("malformed test attempt must insert");
        }

        assert!(
            store
                .claim_next(
                    JobClaimRequest::new("worker", time(10), 5).expect("claim must be valid")
                )
                .is_err()
        );
    }

    #[test]
    fn claim_rejects_clock_regression_before_creation_and_after_retryable_failure() {
        let mut store = SqliteJobStore::open_in_memory().expect("queue must open");
        store
            .enqueue(job("game:101", 3, 100))
            .expect("enqueue must succeed");

        assert!(
            store
                .claim_next(
                    JobClaimRequest::new("worker", time(10), 5).expect("claim must be valid")
                )
                .is_err()
        );
        let first = claim(&mut store, "worker", 100, 10);
        store
            .fail(
                JobFailure::new(first.into_claim(), time(105), "temporary source error")
                    .expect("failure must be valid"),
            )
            .expect("failure must persist");

        assert!(
            store
                .claim_next(
                    JobClaimRequest::new("worker", time(50), 5).expect("claim must be valid")
                )
                .is_err()
        );
        let record = store
            .job("game:101")
            .expect("job must load")
            .expect("job must exist");
        assert_eq!(
            (record.status(), record.attempt_count(), record.updated_at()),
            (JobStatus::Ready, 1, time(105))
        );
    }

    #[test]
    fn expired_lease_is_recovered_and_a_stale_claim_cannot_finish_new_work() {
        let mut store = SqliteJobStore::open_in_memory().expect("queue must open");
        store
            .enqueue(job("game:101", 3, 1))
            .expect("enqueue must succeed");
        let first = claim(&mut store, "worker-a", 10, 5);
        assert!(
            store
                .claim_next(
                    JobClaimRequest::new("worker-b", time(45), 5).expect("claim must be valid")
                )
                .expect("expired claim recovery must query")
                .is_none()
        );
        let second = claim(&mut store, "worker-b", 75, 5);

        assert_eq!(first.claim().claim_token(), 1);
        assert_eq!(second.claim().claim_token(), 2);
        assert!(
            store
                .complete(
                    JobCompletion::new(first.into_claim(), time(11))
                        .expect("completion must be valid"),
                )
                .is_err()
        );
        store
            .complete(
                JobCompletion::new(second.into_claim(), time(76))
                    .expect("completion must be valid"),
            )
            .expect("current claim must complete");

        let attempts = store.attempts("game:101").expect("attempts must load");
        assert_eq!(
            attempts.iter().map(JobAttempt::outcome).collect::<Vec<_>>(),
            [JobAttemptOutcome::Expired, JobAttemptOutcome::Succeeded]
        );
        assert_eq!(
            store
                .job("game:101")
                .expect("job must load")
                .expect("job must exist")
                .status(),
            JobStatus::Succeeded
        );
    }

    #[test]
    fn retry_ceiling_transitions_to_failed_and_preserves_terminal_history() {
        let mut store = SqliteJobStore::open_in_memory().expect("queue must open");
        store
            .enqueue(job("game:101", 2, 1))
            .expect("enqueue must succeed");
        let first = claim(&mut store, "worker", 10, 5);
        assert_eq!(
            store
                .fail(
                    JobFailure::new(first.into_claim(), time(11), "temporary source error")
                        .expect("failure must be valid"),
                )
                .expect("retryable failure must persist"),
            JobFailureResult::ReadyForRetry
        );
        let second = claim(&mut store, "worker", 41, 5);
        assert_eq!(
            store
                .fail(
                    JobFailure::new(second.into_claim(), time(42), "permanent source error")
                        .expect("failure must be valid"),
                )
                .expect("terminal failure must persist"),
            JobFailureResult::Failed
        );
        assert!(
            store
                .claim_next(
                    JobClaimRequest::new("worker", time(43), 5).expect("claim must be valid")
                )
                .expect("empty claim must succeed")
                .is_none()
        );

        let record = store
            .job("game:101")
            .expect("job must load")
            .expect("job must exist");
        assert_eq!(
            (record.status(), record.attempt_count()),
            (JobStatus::Failed, 2)
        );
        assert_eq!(record.last_error(), Some("permanent source error"));
        assert_eq!(
            store
                .attempts("game:101")
                .expect("attempts must load")
                .iter()
                .map(JobAttempt::outcome)
                .collect::<Vec<_>>(),
            [
                JobAttemptOutcome::RetryableFailure,
                JobAttemptOutcome::TerminalFailure,
            ]
        );
    }

    #[test]
    fn retry_backoff_persists_across_reopen_and_success_clears_retry_eligibility() {
        let database = TemporaryDatabase::new("retry-backoff-reopen");
        {
            let mut store = database.open();
            store
                .enqueue(job("game:101", 3, 10))
                .expect("enqueue must succeed");
            let first = claim(&mut store, "worker", 10, 5);
            assert_eq!(
                store
                    .fail(
                        JobFailure::new(first.into_claim(), time(11), "timeout")
                            .expect("failure must be valid"),
                    )
                    .expect("failure must persist"),
                JobFailureResult::ReadyForRetry
            );
            let record = store
                .job("game:101")
                .expect("job must load")
                .expect("job must exist");
            assert_eq!(record.retry_not_before(), Some(time(41)));
            assert!(
                store
                    .claim_next(
                        JobClaimRequest::new("worker", time(40), 5).expect("claim must be valid")
                    )
                    .expect("early claim must query")
                    .is_none()
            );
        }

        let mut reopened = database.open();
        assert!(
            reopened
                .claim_next(
                    JobClaimRequest::new("worker", time(40), 5).expect("claim must be valid")
                )
                .expect("reopened early claim must query")
                .is_none()
        );
        let retry = claim(&mut reopened, "worker", 41, 5);
        reopened
            .complete(
                JobCompletion::new(retry.into_claim(), time(42)).expect("completion must be valid"),
            )
            .expect("retry success must persist");
        assert_eq!(
            reopened
                .job("game:101")
                .expect("job must load")
                .expect("job must exist")
                .retry_not_before(),
            None
        );
    }

    #[test]
    fn transient_timeout_rate_limit_and_provider_failures_share_the_durable_schedule() {
        for (identity, error) in [
            ("timeout", "timeout"),
            ("rate-limit", "source returned 429"),
            ("provider", "provider unavailable"),
        ] {
            let mut store = SqliteJobStore::open_in_memory().expect("queue must open");
            store
                .enqueue(job(identity, 2, 100))
                .expect("enqueue must succeed");
            let claimed = claim(&mut store, "worker", 100, 5);
            assert_eq!(
                store
                    .fail(
                        JobFailure::new(claimed.into_claim(), time(101), error)
                            .expect("failure must be valid"),
                    )
                    .expect("failure must persist"),
                JobFailureResult::ReadyForRetry
            );
            let record = store
                .job(identity)
                .expect("job must load")
                .expect("job must exist");
            assert_eq!(record.retry_not_before(), Some(time(131)));
            assert!(
                store
                    .claim_next(
                        JobClaimRequest::new("worker", time(130), 5).expect("claim must be valid")
                    )
                    .expect("early claim must query")
                    .is_none()
            );
        }
    }

    #[test]
    fn source_lane_pacing_survives_reopen_without_delaying_other_queue_state() {
        let database = TemporaryDatabase::new("source-lane-pacing");
        let pacing =
            gamepulse_application::JobClaimPacing::new("source", 2).expect("pacing must be valid");
        {
            let mut store = database.open();
            for identity in ["source-a", "source-b"] {
                store
                    .enqueue(
                        JobRequest::new(
                            identity,
                            RuntimeJobType::SourceHourlyDiscovery.as_str(),
                            "hour-slot:0",
                            2,
                            time(1),
                        )
                        .expect("source job must be valid"),
                    )
                    .expect("source job must enqueue");
            }
            assert!(
                store
                    .claim_next_matching(
                        JobClaimRequest::new("source-worker", time(10), 5)
                            .expect("claim must be valid")
                            .with_pacing(pacing.clone()),
                        &[RuntimeJobType::SourceHourlyDiscovery],
                    )
                    .expect("first paced claim must succeed")
                    .is_some()
            );
        }

        let mut reopened = database.open();
        assert!(
            reopened
                .claim_next_matching(
                    JobClaimRequest::new("source-worker", time(11), 5)
                        .expect("claim must be valid")
                        .with_pacing(pacing.clone()),
                    &[RuntimeJobType::SourceHourlyDiscovery],
                )
                .expect("paced early claim must query")
                .is_none()
        );
        assert!(
            reopened
                .claim_next_matching(
                    JobClaimRequest::new("source-worker", time(12), 5)
                        .expect("claim must be valid")
                        .with_pacing(pacing),
                    &[RuntimeJobType::SourceHourlyDiscovery],
                )
                .expect("paced later claim must query")
                .is_some()
        );
    }

    #[test]
    fn file_backed_terminal_state_and_history_survive_reopen() {
        let database = TemporaryDatabase::new("reopen");
        {
            let mut store = database.open();
            store
                .enqueue(job("game:101", 1, 1))
                .expect("enqueue must succeed");
            let claimed = claim(&mut store, "worker", 2, 5);
            store
                .complete(
                    JobCompletion::new(claimed.into_claim(), time(3))
                        .expect("completion must be valid"),
                )
                .expect("completion must persist");
        }

        let mut reopened = database.open();
        let record = reopened
            .job("game:101")
            .expect("job must load")
            .expect("job must exist");
        assert_eq!(record.status(), JobStatus::Succeeded);
        assert_eq!(record.terminal_at(), Some(time(3)));
        assert_eq!(
            reopened.attempts("game:101").expect("attempts must load")[0].outcome(),
            JobAttemptOutcome::Succeeded
        );
    }

    #[test]
    fn terminal_expired_claim_is_failed_at_the_retry_ceiling() {
        let mut store = SqliteJobStore::open_in_memory().expect("queue must open");
        store
            .enqueue(job("game:101", 1, 1))
            .expect("enqueue must succeed");
        let _first = claim(&mut store, "worker-a", 10, 5);

        assert!(
            store
                .claim_next(
                    JobClaimRequest::new("worker-b", time(15), 5).expect("claim must be valid")
                )
                .expect("claim must succeed")
                .is_none()
        );
        let record = store
            .job("game:101")
            .expect("job must load")
            .expect("job must exist");
        assert_eq!(record.status(), JobStatus::Failed);
        assert_eq!(record.last_error(), Some(EXPIRED_LEASE_ERROR));
        assert_eq!(
            store.attempts("game:101").expect("attempts must load")[0].outcome(),
            JobAttemptOutcome::Expired
        );
    }

    #[test]
    fn lifecycle_and_attempt_history_roll_back_together_when_finalization_fails() {
        let mut store = SqliteJobStore::open_in_memory().expect("queue must open");
        store
            .enqueue(job("game:101", 2, 1))
            .expect("enqueue must succeed");
        let claimed = claim(&mut store, "worker", 10, 5);
        store.install_attempt_finalize_failure_for_test();

        assert!(
            store
                .fail(
                    JobFailure::new(claimed.into_claim(), time(11), "temporary source error")
                        .expect("failure must be valid"),
                )
                .is_err()
        );
        let record = store
            .job("game:101")
            .expect("job must load")
            .expect("job must exist");
        assert_eq!(record.status(), JobStatus::Claimed);
        assert_eq!(
            store.attempts("game:101").expect("attempts must load")[0].outcome(),
            JobAttemptOutcome::Active
        );
    }

    #[test]
    fn claim_and_attempt_history_roll_back_together_when_attempt_creation_fails() {
        let mut store = SqliteJobStore::open_in_memory().expect("queue must open");
        store
            .enqueue(job("game:101", 2, 1))
            .expect("enqueue must succeed");
        store.install_attempt_insert_failure_for_test();

        assert!(
            store
                .claim_next(
                    JobClaimRequest::new("worker", time(10), 5).expect("claim must be valid")
                )
                .is_err()
        );
        let record = store
            .job("game:101")
            .expect("job must load")
            .expect("job must exist");
        assert_eq!(
            (record.status(), record.attempt_count()),
            (JobStatus::Ready, 0)
        );
        assert!(
            store
                .attempts("game:101")
                .expect("attempts must load")
                .is_empty()
        );
    }

    #[test]
    fn separate_stores_reject_a_stale_worker_after_lease_recovery() {
        let database = TemporaryDatabase::new("stale-worker");
        let mut first_store = database.open();
        let mut second_store = database.open();
        first_store
            .enqueue(job("game:101", 3, 1))
            .expect("enqueue must succeed");
        let first = claim(&mut first_store, "worker-a", 10, 5);
        assert!(
            second_store
                .claim_next(
                    JobClaimRequest::new("worker-b", time(45), 5).expect("claim must be valid")
                )
                .expect("expired claim recovery must query")
                .is_none()
        );
        let second = claim(&mut second_store, "worker-b", 75, 5);

        assert!(
            first_store
                .fail(
                    JobFailure::new(first.into_claim(), time(11), "stale failure")
                        .expect("failure must be valid"),
                )
                .is_err()
        );
        second_store
            .complete(
                JobCompletion::new(second.into_claim(), time(76))
                    .expect("completion must be valid"),
            )
            .expect("new owner must complete");
        assert_eq!(
            first_store
                .job("game:101")
                .expect("job must load")
                .expect("job must exist")
                .status(),
            JobStatus::Succeeded
        );
    }
}
