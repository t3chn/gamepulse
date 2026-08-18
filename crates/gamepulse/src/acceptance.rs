#![forbid(unsafe_code)]

//! Explicit, aggregate-only evaluator acceptance orchestration.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gamepulse_application::{
    AcceptanceCycleReadPort, AcceptanceCycleSnapshot, DAILY_CRAWL_SELECTION_LIMIT,
    FailureCategoryCounts, JobStore, WorkerFailureCategory,
};

use crate::runtime::{Runtime, RuntimeClock, RuntimeTaskOutcome};

pub const ACCEPTANCE_SUBCOMMAND: &str = "acceptance-once";
pub const ACCEPTANCE_SCHEMA_VERSION: &str = "gamepulse.acceptance.v1";
pub const ACCEPTANCE_HELP: &str = concat!(
    "Usage:\n",
    "  gamepulse acceptance-once --database <ABSOLUTE_DATABASE_PATH> --deadline-seconds <POSITIVE_SECONDS> [--target 20]\n\n",
    "Run one evaluator acceptance cycle without starting the HTTP server or scheduler loop.\n\n",
    "Options:\n",
    "  --database <ABSOLUTE_DATABASE_PATH>  Required fresh absolute SQLite path.\n",
    "  --deadline-seconds <POSITIVE_SECONDS>  Required positive hard deadline.\n",
    "  --target 20  Optional explicit mandatory target; only 20 is accepted.\n",
    "  --help  Show this help and exit.\n",
);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EntryCommand {
    Serve,
    AcceptanceHelp,
    Acceptance(AcceptanceCommand),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptanceCommand {
    database_path: PathBuf,
    target: usize,
    deadline: Duration,
}

impl AcceptanceCommand {
    pub fn new(
        database_path: PathBuf,
        target: usize,
        deadline_seconds: u64,
    ) -> Result<Self, CommandParseError> {
        if !database_path.is_absolute()
            || target != DAILY_CRAWL_SELECTION_LIMIT
            || deadline_seconds == 0
        {
            return Err(CommandParseError);
        }
        Ok(Self {
            database_path,
            target,
            deadline: Duration::from_secs(deadline_seconds),
        })
    }

    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    pub const fn target(&self) -> usize {
        self.target
    }

    pub const fn deadline(&self) -> Duration {
        self.deadline
    }
}

/// Parse only the ordinary server mode or the explicit opt-in acceptance command.
pub fn parse_entry_command(
    arguments: impl IntoIterator<Item = OsString>,
) -> Result<EntryCommand, CommandParseError> {
    let mut arguments = arguments.into_iter();
    let Some(command) = arguments.next() else {
        return Ok(EntryCommand::Serve);
    };
    if command.as_os_str() != OsStr::new(ACCEPTANCE_SUBCOMMAND) {
        return Err(CommandParseError);
    }

    let first_argument = arguments.next();
    if matches!(
        first_argument.as_ref(),
        Some(argument) if argument.as_os_str() == OsStr::new("--help")
    ) {
        return if arguments.next().is_none() {
            Ok(EntryCommand::AcceptanceHelp)
        } else {
            Err(CommandParseError)
        };
    }
    let mut arguments = first_argument.into_iter().chain(arguments);

    let mut database_path = None;
    let mut deadline_seconds = None;
    let mut target = None;
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--database") if database_path.is_none() => {
                database_path = arguments.next().map(PathBuf::from);
            }
            Some("--deadline-seconds") if deadline_seconds.is_none() => {
                deadline_seconds = arguments
                    .next()
                    .and_then(|value| value.into_string().ok())
                    .and_then(|value| value.parse::<u64>().ok());
            }
            Some("--target") if target.is_none() => {
                let target_value = arguments.next().ok_or(CommandParseError)?;
                let target_value = target_value.into_string().map_err(|_| CommandParseError)?;
                target = Some(
                    target_value
                        .parse::<usize>()
                        .map_err(|_| CommandParseError)?,
                );
            }
            _ => return Err(CommandParseError),
        }
    }
    let database_path = database_path.ok_or(CommandParseError)?;
    let deadline_seconds = deadline_seconds.ok_or(CommandParseError)?;
    let target = target.unwrap_or(DAILY_CRAWL_SELECTION_LIMIT);
    AcceptanceCommand::new(database_path, target, deadline_seconds).map(EntryCommand::Acceptance)
}

/// The fixed internal terminal categories for the machine-readable acceptance report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcceptanceTerminal {
    Complete,
    MandatoryJobFailure,
    TargetFailure,
    Deadline,
    ConfigurationFailure,
    RuntimeFailure,
}

impl AcceptanceTerminal {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::MandatoryJobFailure => "mandatory_job_failure",
            Self::TargetFailure => "target_failure",
            Self::Deadline => "deadline",
            Self::ConfigurationFailure => "configuration_failure",
            Self::RuntimeFailure => "runtime_failure",
        }
    }

    pub const fn exit_code(self) -> i32 {
        match self {
            Self::Complete => 0,
            Self::RuntimeFailure => 1,
            Self::MandatoryJobFailure
            | Self::TargetFailure
            | Self::Deadline
            | Self::ConfigurationFailure => 3,
        }
    }
}

/// A safe, aggregate-only acceptance result. Its serializer uses no caller or database values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcceptanceReport {
    terminal: AcceptanceTerminal,
    target: usize,
    snapshot: AcceptanceCycleSnapshot,
    observed_failures: FailureCategoryCounts,
    runtime_millis: u64,
}

impl AcceptanceReport {
    pub const fn new(
        terminal: AcceptanceTerminal,
        target: usize,
        snapshot: AcceptanceCycleSnapshot,
        runtime_millis: u64,
    ) -> Self {
        Self {
            terminal,
            target,
            snapshot,
            observed_failures: FailureCategoryCounts::zero(),
            runtime_millis,
        }
    }

    pub const fn with_observed_failures(
        terminal: AcceptanceTerminal,
        target: usize,
        snapshot: AcceptanceCycleSnapshot,
        observed_failures: FailureCategoryCounts,
        runtime_millis: u64,
    ) -> Self {
        Self {
            terminal,
            target,
            snapshot,
            observed_failures,
            runtime_millis,
        }
    }

    pub const fn terminal(&self) -> AcceptanceTerminal {
        self.terminal
    }

    pub const fn exit_code(&self) -> i32 {
        self.terminal().exit_code()
    }

    #[allow(dead_code)]
    pub const fn snapshot(&self) -> AcceptanceCycleSnapshot {
        self.snapshot
    }

    #[allow(dead_code)]
    pub const fn observed_failures(&self) -> FailureCategoryCounts {
        self.observed_failures
    }

    /// Render the one allowed stdout object. No source-derived strings are interpolated.
    pub fn to_json(self) -> String {
        let failures = self.snapshot.failures();
        let runtime = usize::from(self.terminal == AcceptanceTerminal::RuntimeFailure);
        let deadline = usize::from(self.terminal == AcceptanceTerminal::Deadline);
        let target = usize::from(self.terminal == AcceptanceTerminal::TargetFailure);
        format!(
            concat!(
                "{{\"schema_version\":\"{schema}\",",
                "\"terminal_outcome\":\"{terminal}\",",
                "\"target\":{target_count},",
                "\"selected\":{selected},",
                "\"attempted\":{attempted},",
                "\"persisted\":{persisted},",
                "\"complete_video\":{complete_video},",
                "\"summary_readiness\":{{\"ready\":{summary_ready},",
                "\"pending_or_missing\":{summary_pending}}},",
                "\"failure_categories\":{{",
                "\"source_review_continuation_link\":{source_link},",
                "\"source_other_mandatory_stage\":{source_other},",
                "\"summary\":{summary},",
                "\"runtime\":{runtime},",
                "\"deadline\":{deadline},",
                "\"target\":{target_failure}}},",
                "\"observed_failure_categories\":{{",
                "\"missing_required_video\":{missing_required_video},",
                "\"source_transport_or_contract\":{source_transport_or_contract},",
                "\"persistence_or_queue\":{persistence_or_queue},",
                "\"other_mandatory\":{other_mandatory}}},",
                "\"runtime_ms\":{runtime_millis}}}"
            ),
            schema = ACCEPTANCE_SCHEMA_VERSION,
            terminal = self.terminal.as_str(),
            target_count = self.target,
            selected = self.snapshot.selected(),
            attempted = self.snapshot.source_ingestion().attempted(),
            persisted = self.snapshot.persisted(),
            complete_video = self.snapshot.complete_video(),
            summary_ready = self.snapshot.summaries_ready(),
            summary_pending = self.snapshot.summaries_pending_or_missing(),
            source_link = failures.source_review_continuation_link(),
            source_other = failures.source_other_mandatory_stage(),
            summary = failures.summary(),
            runtime = runtime,
            deadline = deadline,
            target_failure = target,
            missing_required_video = self.observed_failures.missing_required_video(),
            source_transport_or_contract = self.observed_failures.source_transport_or_contract(),
            persistence_or_queue = self.observed_failures.persistence_or_queue(),
            other_mandatory = self.observed_failures.other_mandatory(),
            runtime_millis = self.runtime_millis,
        )
    }
}

/// Run exactly one scheduler enqueue followed by the cycle's mandatory worker jobs.
///
/// No timer-driven production loop, retry, or second scheduler enqueue is used here.
/// The fresh database precondition means every accepted source-ingestion and review-summary job
/// is scoped to this one cycle.
pub async fn run_acceptance_once<S, C, O>(
    source_runtime: &mut Runtime<S, C>,
    summary_runtime: &mut Runtime<S, C>,
    observation: &mut O,
    command: &AcceptanceCommand,
) -> AcceptanceReport
where
    S: JobStore + Send + 'static,
    S::Error: Send + 'static,
    C: RuntimeClock,
    O: AcceptanceCycleReadPort,
{
    let started = Instant::now();
    let mut observed_failures = FailureCategoryCounts::zero();
    let mut terminal = match tokio::time::timeout(
        command.deadline(),
        execute_cycle(
            source_runtime,
            summary_runtime,
            observation,
            command.target(),
            &mut observed_failures,
        ),
    )
    .await
    {
        Ok(Ok(())) => AcceptanceTerminal::Complete,
        Ok(Err(terminal)) => terminal,
        Err(_) => {
            source_runtime.abort_active();
            summary_runtime.abort_active();
            AcceptanceTerminal::Deadline
        }
    };
    let snapshot = match observation.acceptance_cycle_snapshot() {
        Ok(snapshot) => snapshot,
        Err(_) => {
            observed_failures.increment(WorkerFailureCategory::PersistenceOrQueue);
            terminal = AcceptanceTerminal::RuntimeFailure;
            AcceptanceCycleSnapshot::default()
        }
    };
    AcceptanceReport::with_observed_failures(
        terminal,
        command.target(),
        snapshot,
        observed_failures,
        elapsed_millis(started.elapsed()),
    )
}

async fn execute_cycle<S, C, O>(
    source_runtime: &mut Runtime<S, C>,
    summary_runtime: &mut Runtime<S, C>,
    observation: &mut O,
    target: usize,
    observed_failures: &mut FailureCategoryCounts,
) -> Result<(), AcceptanceTerminal>
where
    S: JobStore + Send + 'static,
    S::Error: Send + 'static,
    C: RuntimeClock,
    O: AcceptanceCycleReadPort,
{
    match source_runtime.schedule_hourly().map_err(|_| {
        observed_failures.increment(WorkerFailureCategory::PersistenceOrQueue);
        AcceptanceTerminal::RuntimeFailure
    })? {
        crate::runtime::SchedulerOutcome::Enqueued => {}
        crate::runtime::SchedulerOutcome::Duplicate | crate::runtime::SchedulerOutcome::Stopped => {
            return Err(AcceptanceTerminal::TargetFailure);
        }
    }

    let discovery = source_runtime.dispatch_available().map_err(|_| {
        observed_failures.increment(WorkerFailureCategory::PersistenceOrQueue);
        AcceptanceTerminal::RuntimeFailure
    })?;
    observed_failures.merge(discovery.observed_failures());
    if discovery.claimed != 1 {
        return Err(AcceptanceTerminal::TargetFailure);
    }
    let discovery = source_runtime.join_all().await.map_err(|_| {
        observed_failures.increment(WorkerFailureCategory::PersistenceOrQueue);
        AcceptanceTerminal::RuntimeFailure
    })?;
    observed_failures.merge(discovery.observed_failures());
    if !all_succeeded(&discovery.settled) {
        return Err(AcceptanceTerminal::MandatoryJobFailure);
    }

    let snapshot = observation.acceptance_cycle_snapshot().map_err(|_| {
        observed_failures.increment(WorkerFailureCategory::PersistenceOrQueue);
        AcceptanceTerminal::RuntimeFailure
    })?;
    if snapshot.selected() > target || snapshot.source_ingestion().total() == 0 {
        return Err(AcceptanceTerminal::TargetFailure);
    }

    drain_source_run(source_runtime, observation, target, observed_failures).await?;
    let summary_target = target
        .checked_mul(2)
        .ok_or(AcceptanceTerminal::TargetFailure)?;
    drain_mandatory_lane(
        summary_runtime,
        observation,
        summary_target,
        Lane::Summary,
        observed_failures,
    )
    .await?;

    let snapshot = observation.acceptance_cycle_snapshot().map_err(|_| {
        observed_failures.increment(WorkerFailureCategory::PersistenceOrQueue);
        AcceptanceTerminal::RuntimeFailure
    })?;
    if snapshot.selected() != target
        || !snapshot.source_ingestion().is_terminal()
        || snapshot.source_ingestion().failed() != 0
        || snapshot.source_ingestion().succeeded() < target
        || snapshot.summaries().total() != summary_target
        || snapshot.summaries().succeeded() != summary_target
        || snapshot.persisted() != target
        || snapshot.complete_video() != target
        || snapshot.summaries_ready() != target
        || snapshot.summaries_pending_or_missing() != 0
    {
        return Err(AcceptanceTerminal::TargetFailure);
    }
    Ok(())
}

async fn drain_source_run<S, C, O>(
    runtime: &mut Runtime<S, C>,
    observation: &mut O,
    target: usize,
    observed_failures: &mut FailureCategoryCounts,
) -> Result<(), AcceptanceTerminal>
where
    S: JobStore + Send + 'static,
    S::Error: Send + 'static,
    C: RuntimeClock,
    O: AcceptanceCycleReadPort,
{
    loop {
        let snapshot = observation.acceptance_cycle_snapshot().map_err(|_| {
            observed_failures.increment(WorkerFailureCategory::PersistenceOrQueue);
            AcceptanceTerminal::RuntimeFailure
        })?;
        let jobs = snapshot.source_ingestion();
        if snapshot.selected() > target || jobs.failed() > 0 {
            return Err(if jobs.failed() > 0 {
                AcceptanceTerminal::MandatoryJobFailure
            } else {
                AcceptanceTerminal::TargetFailure
            });
        }
        if snapshot.selected() == target && jobs.is_terminal() {
            return Ok(());
        }
        let dispatched = runtime.dispatch_available().map_err(|_| {
            observed_failures.increment(WorkerFailureCategory::PersistenceOrQueue);
            AcceptanceTerminal::RuntimeFailure
        })?;
        observed_failures.merge(dispatched.observed_failures());
        if dispatched.claimed == 0 {
            let waited = runtime
                .wait_for_next_claim_eligibility()
                .await
                .map_err(|_| {
                    observed_failures.increment(WorkerFailureCategory::PersistenceOrQueue);
                    AcceptanceTerminal::RuntimeFailure
                })?;
            if waited {
                continue;
            }
            return Err(AcceptanceTerminal::TargetFailure);
        }
        let settled = runtime.join_all().await.map_err(|_| {
            observed_failures.increment(WorkerFailureCategory::PersistenceOrQueue);
            AcceptanceTerminal::RuntimeFailure
        })?;
        observed_failures.merge(settled.observed_failures());
        if !all_succeeded(&settled.settled) {
            return Err(AcceptanceTerminal::MandatoryJobFailure);
        }
    }
}

#[derive(Clone, Copy)]
enum Lane {
    Summary,
}

async fn drain_mandatory_lane<S, C, O>(
    runtime: &mut Runtime<S, C>,
    observation: &mut O,
    expected: usize,
    lane: Lane,
    observed_failures: &mut FailureCategoryCounts,
) -> Result<(), AcceptanceTerminal>
where
    S: JobStore + Send + 'static,
    S::Error: Send + 'static,
    C: RuntimeClock,
    O: AcceptanceCycleReadPort,
{
    loop {
        let snapshot = observation.acceptance_cycle_snapshot().map_err(|_| {
            observed_failures.increment(WorkerFailureCategory::PersistenceOrQueue);
            AcceptanceTerminal::RuntimeFailure
        })?;
        let jobs = match lane {
            Lane::Summary => snapshot.summaries(),
        };
        if jobs.total() != expected {
            return Err(AcceptanceTerminal::TargetFailure);
        }
        if jobs.failed() > 0 {
            return Err(AcceptanceTerminal::MandatoryJobFailure);
        }
        if jobs.is_terminal() {
            return if jobs.succeeded() == expected {
                Ok(())
            } else {
                Err(AcceptanceTerminal::MandatoryJobFailure)
            };
        }

        let dispatched = runtime.dispatch_available().map_err(|_| {
            observed_failures.increment(WorkerFailureCategory::PersistenceOrQueue);
            AcceptanceTerminal::RuntimeFailure
        })?;
        observed_failures.merge(dispatched.observed_failures());
        if dispatched.claimed == 0 {
            return Err(AcceptanceTerminal::TargetFailure);
        }
        let settled = runtime.join_all().await.map_err(|_| {
            observed_failures.increment(WorkerFailureCategory::PersistenceOrQueue);
            AcceptanceTerminal::RuntimeFailure
        })?;
        observed_failures.merge(settled.observed_failures());
        match all_succeeded(&settled.settled) {
            true => {}
            false => return Err(AcceptanceTerminal::MandatoryJobFailure),
        }
    }
}

fn all_succeeded(outcomes: &[RuntimeTaskOutcome]) -> bool {
    !outcomes.is_empty()
        && outcomes
            .iter()
            .all(|outcome| matches!(outcome, RuntimeTaskOutcome::Succeeded))
}

fn elapsed_millis(elapsed: Duration) -> u64 {
    elapsed.as_millis().try_into().unwrap_or(u64::MAX)
}

/// Whether the caller path can be used without overwriting a database file.
pub fn database_path_is_fresh(path: &Path) -> bool {
    !path.exists()
        && !sqlite_sidecar_path(path, "-journal").exists()
        && !sqlite_sidecar_path(path, "-shm").exists()
        && !sqlite_sidecar_path(path, "-wal").exists()
}

fn sqlite_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandParseError;
