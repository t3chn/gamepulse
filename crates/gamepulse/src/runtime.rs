#![forbid(unsafe_code)]

use std::fmt;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gamepulse_application::{
    ClaimedJob, HourlyJobSchedule, JobClaimRequest, JobCompletion, JobEnqueueResult, JobFailure,
    JobFailureResult, JobHandlerFailure, JobHandlerRegistry, JobHandlerResult, JobInputError,
    JobStore, JobTimestamp, TypedJob,
};
use tokio::task::JoinSet;
use tokio::time::MissedTickBehavior;

const UNSUPPORTED_JOB_TYPE: &str = "unsupported M006 job type";
const MISSING_TYPED_HANDLER: &str = "no typed M006 handler is registered";
const INVALID_HANDLER_FAILURE: &str = "worker handler returned an invalid failure";

/// Clock port for deterministic scheduler and lease tests.
pub trait RuntimeClock: Send + Sync + 'static {
    fn now(&self) -> Result<JobTimestamp, RuntimeClockError>;
}

/// The production clock converts the Unix epoch into the application's validated timestamp type.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemRuntimeClock;

impl RuntimeClock for SystemRuntimeClock {
    fn now(&self) -> Result<JobTimestamp, RuntimeClockError> {
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| RuntimeClockError::BeforeUnixEpoch)?;
        let seconds = i64::try_from(elapsed.as_secs()).map_err(|_| RuntimeClockError::Overflow)?;
        JobTimestamp::new(seconds).map_err(|_| RuntimeClockError::Overflow)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeClockError {
    BeforeUnixEpoch,
    Overflow,
}

impl fmt::Display for RuntimeClockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BeforeUnixEpoch => formatter.write_str("runtime clock predates the Unix epoch"),
            Self::Overflow => formatter.write_str("runtime clock cannot be represented"),
        }
    }
}

impl std::error::Error for RuntimeClockError {}

/// The process-local limits and durable job family configured at the composition root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeConfig {
    worker_id: String,
    lease_seconds: i64,
    concurrency_limit: usize,
    hourly_schedule: HourlyJobSchedule,
}

impl RuntimeConfig {
    pub fn new(
        worker_id: impl Into<String>,
        lease_seconds: i64,
        concurrency_limit: usize,
        hourly_schedule: HourlyJobSchedule,
    ) -> Result<Self, RuntimeConfigError> {
        let worker_id = worker_id.into();
        JobClaimRequest::new(
            worker_id.clone(),
            JobTimestamp::new(0).expect("zero timestamp must be valid"),
            lease_seconds,
        )
        .map_err(RuntimeConfigError::InvalidJobInput)?;
        if concurrency_limit == 0 {
            return Err(RuntimeConfigError::ZeroConcurrencyLimit);
        }
        Ok(Self {
            worker_id,
            lease_seconds,
            concurrency_limit,
            hourly_schedule,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeConfigError {
    InvalidJobInput(JobInputError),
    ZeroConcurrencyLimit,
}

impl fmt::Display for RuntimeConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJobInput(_) => formatter.write_str("invalid runtime queue configuration"),
            Self::ZeroConcurrencyLimit => {
                formatter.write_str("runtime concurrency limit must be positive")
            }
        }
    }
}

impl std::error::Error for RuntimeConfigError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchedulerOutcome {
    Enqueued,
    Duplicate,
    Stopped,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DispatchReport {
    pub claimed: usize,
    pub settled: Vec<RuntimeTaskOutcome>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeTaskOutcome {
    Succeeded,
    Failed(JobFailureResult),
    CompletionRejected,
    FailureRejected,
    ClockUnavailable,
    StoreUnavailable,
}

/// Bounded, in-process M006 scheduler and durable queue dispatcher.
///
/// The `JoinSet` is process-local coordination only. Every queued job, claim,
/// completion, failure, retry, and lease recovery remains owned by `JobStore`.
pub struct Runtime<S, C>
where
    S: JobStore + Send + 'static,
    S::Error: Send + 'static,
    C: RuntimeClock,
{
    store: Arc<Mutex<S>>,
    clock: Arc<C>,
    config: RuntimeConfig,
    handlers: Arc<JobHandlerRegistry>,
    accepting_work: bool,
    tasks: JoinSet<RuntimeTaskOutcome>,
}

impl<S, C> Runtime<S, C>
where
    S: JobStore + Send + 'static,
    S::Error: Send + 'static,
    C: RuntimeClock,
{
    pub fn new(
        store: Arc<Mutex<S>>,
        clock: Arc<C>,
        config: RuntimeConfig,
        handlers: Arc<JobHandlerRegistry>,
    ) -> Self {
        Self {
            store,
            clock,
            config,
            handlers,
            accepting_work: true,
            tasks: JoinSet::new(),
        }
    }

    /// Enqueue the current hour's job through durable identity/deduplication.
    pub fn schedule_hourly(&mut self) -> Result<SchedulerOutcome, RuntimeError> {
        if !self.accepting_work {
            return Ok(SchedulerOutcome::Stopped);
        }
        let request = self
            .config
            .hourly_schedule
            .request_for(self.clock.now().map_err(|_| RuntimeError::Clock)?)
            .map_err(|_| RuntimeError::InvalidJobInput)?;
        let result = self
            .store()?
            .enqueue(request)
            .map_err(|_| RuntimeError::StoreUnavailable)?;
        Ok(match result {
            JobEnqueueResult::Enqueued(_) => SchedulerOutcome::Enqueued,
            JobEnqueueResult::Duplicate(_) => SchedulerOutcome::Duplicate,
        })
    }

    /// Claim at most the configured remaining capacity and start the matching typed handlers.
    pub fn dispatch_available(&mut self) -> Result<DispatchReport, RuntimeError> {
        let mut report = self.reap_finished()?;
        if !self.accepting_work {
            return Ok(report);
        }
        let claimed_at = self.clock.now().map_err(|_| RuntimeError::Clock)?;

        while self.accepting_work && self.tasks.len() < self.config.concurrency_limit {
            let claim_request = JobClaimRequest::new(
                self.config.worker_id.clone(),
                claimed_at,
                self.config.lease_seconds,
            )
            .map_err(|_| RuntimeError::InvalidJobInput)?;
            let claimed = self
                .store()?
                .claim_next(claim_request)
                .map_err(|_| RuntimeError::StoreUnavailable)?;
            let Some(claimed) = claimed else {
                break;
            };
            report.claimed += 1;
            self.tasks.spawn(execute_claim(
                Arc::clone(&self.store),
                Arc::clone(&self.clock),
                Arc::clone(&self.handlers),
                claimed,
            ));
        }

        Ok(report)
    }

    /// Run one production scheduler turn without waiting for wall-clock time.
    pub fn tick(&mut self) -> Result<(SchedulerOutcome, DispatchReport), RuntimeError> {
        let scheduled = self.schedule_hourly()?;
        let dispatched = self.dispatch_available()?;
        Ok((scheduled, dispatched))
    }

    /// Stop future scheduler and dispatcher work. Existing tasks may only settle through `JobStore`.
    pub fn begin_shutdown(&mut self) {
        self.accepting_work = false;
    }

    /// Join every active execution task after stopping scheduling and new claims.
    pub async fn shutdown(&mut self) -> Result<DispatchReport, RuntimeError> {
        self.begin_shutdown();
        self.join_all().await
    }

    /// Join tasks already started by the dispatcher without changing the acceptance state.
    pub async fn join_all(&mut self) -> Result<DispatchReport, RuntimeError> {
        let mut report = self.reap_finished()?;
        while let Some(joined) = self.tasks.join_next().await {
            report
                .settled
                .push(joined.map_err(|_| RuntimeError::TaskJoinFailed)?);
        }
        Ok(report)
    }

    /// Run the production hourly loop until the caller signals graceful shutdown.
    pub async fn run_until_shutdown<F>(&mut self, shutdown_signal: F) -> Result<(), RuntimeError>
    where
        F: Future<Output = ()>,
    {
        let mut interval = tokio::time::interval(Duration::from_secs(
            gamepulse_application::HOURLY_SCHEDULE_SECONDS as u64,
        ));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        tokio::pin!(shutdown_signal);

        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_signal => {
                    self.shutdown().await?;
                    return Ok(());
                }
                completed = self.tasks.join_next(), if !self.tasks.is_empty() => {
                    if let Some(completed) = completed {
                        completed.map_err(|_| RuntimeError::TaskJoinFailed)?;
                    }
                    if self.accepting_work {
                        self.dispatch_available()?;
                    }
                }
                _ = interval.tick() => {
                    self.tick()?;
                }
            }
        }
    }

    fn reap_finished(&mut self) -> Result<DispatchReport, RuntimeError> {
        let mut report = DispatchReport::default();
        while let Some(joined) = self.tasks.try_join_next() {
            report
                .settled
                .push(joined.map_err(|_| RuntimeError::TaskJoinFailed)?);
        }
        Ok(report)
    }

    fn store(&self) -> Result<MutexGuard<'_, S>, RuntimeError> {
        self.store.lock().map_err(|_| RuntimeError::StorePoisoned)
    }
}

async fn execute_claim<S, C>(
    store: Arc<Mutex<S>>,
    clock: Arc<C>,
    handlers: Arc<JobHandlerRegistry>,
    claimed: ClaimedJob,
) -> RuntimeTaskOutcome
where
    S: JobStore + Send + 'static,
    S::Error: Send + 'static,
    C: RuntimeClock,
{
    let Some(job) = TypedJob::from_record(claimed.job()) else {
        return settle_failure(
            store,
            clock,
            claimed,
            JobHandlerFailure::new(UNSUPPORTED_JOB_TYPE),
        );
    };
    let Some(handler) = handlers.handler(job.job_type()) else {
        return settle_failure(
            store,
            clock,
            claimed,
            JobHandlerFailure::new(MISSING_TYPED_HANDLER),
        );
    };

    match handler.handle(job).await {
        JobHandlerResult::Succeeded => settle_success(store, clock, claimed),
        JobHandlerResult::Failed(error) => settle_failure(store, clock, claimed, error),
    }
}

fn settle_success<S, C>(
    store: Arc<Mutex<S>>,
    clock: Arc<C>,
    claimed: ClaimedJob,
) -> RuntimeTaskOutcome
where
    S: JobStore + Send + 'static,
    S::Error: Send + 'static,
    C: RuntimeClock,
{
    let completed_at = match clock.now() {
        Ok(value) => value,
        Err(_) => return RuntimeTaskOutcome::ClockUnavailable,
    };
    let completion = match JobCompletion::new(claimed.into_claim(), completed_at) {
        Ok(value) => value,
        Err(_) => return RuntimeTaskOutcome::CompletionRejected,
    };
    match store.lock() {
        Ok(mut store) => match store.complete(completion) {
            Ok(()) => RuntimeTaskOutcome::Succeeded,
            Err(_) => RuntimeTaskOutcome::CompletionRejected,
        },
        Err(_) => RuntimeTaskOutcome::StoreUnavailable,
    }
}

fn settle_failure<S, C>(
    store: Arc<Mutex<S>>,
    clock: Arc<C>,
    claimed: ClaimedJob,
    error: JobHandlerFailure,
) -> RuntimeTaskOutcome
where
    S: JobStore + Send + 'static,
    S::Error: Send + 'static,
    C: RuntimeClock,
{
    let failed_at = match clock.now() {
        Ok(value) => value,
        Err(_) => return RuntimeTaskOutcome::ClockUnavailable,
    };
    let claim = claimed.into_claim();
    let failure = JobFailure::new(claim.clone(), failed_at, error.message())
        .or_else(|_| JobFailure::new(claim, failed_at, INVALID_HANDLER_FAILURE));
    let Ok(failure) = failure else {
        return RuntimeTaskOutcome::FailureRejected;
    };
    match store.lock() {
        Ok(mut store) => match store.fail(failure) {
            Ok(result) => RuntimeTaskOutcome::Failed(result),
            Err(_) => RuntimeTaskOutcome::FailureRejected,
        },
        Err(_) => RuntimeTaskOutcome::StoreUnavailable,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeError {
    Clock,
    InvalidJobInput,
    StoreUnavailable,
    StorePoisoned,
    TaskJoinFailed,
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Clock => formatter.write_str("runtime clock is unavailable"),
            Self::InvalidJobInput => formatter.write_str("runtime queue input is invalid"),
            Self::StoreUnavailable | Self::StorePoisoned => {
                formatter.write_str("runtime durable queue operation failed")
            }
            Self::TaskJoinFailed => formatter.write_str("runtime task did not join cleanly"),
        }
    }
}

impl std::error::Error for RuntimeError {}
