#![forbid(unsafe_code)]

#[allow(dead_code)]
#[path = "../src/runtime.rs"]
mod runtime;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use gamepulse_application::{
    HourlyJobSchedule, JobAttemptOutcome, JobEnqueueResult, JobFailureResult, JobHandler,
    JobHandlerFuture, JobHandlerRegistry, JobHandlerResult, JobRequest, JobStatus, JobStore,
    JobTimestamp, RuntimeJobType, TypedJob,
};
use gamepulse_storage_sqlite::SqliteJobStore;
use runtime::{
    Runtime, RuntimeClock, RuntimeClockError, RuntimeConfig, RuntimeTaskOutcome, SchedulerOutcome,
};
use tokio::sync::{Notify, oneshot};

#[derive(Debug)]
struct ManualClock {
    seconds: AtomicI64,
}

impl ManualClock {
    fn new(seconds: i64) -> Self {
        Self {
            seconds: AtomicI64::new(seconds),
        }
    }

    fn set(&self, seconds: i64) {
        self.seconds.store(seconds, Ordering::SeqCst);
    }
}

impl RuntimeClock for ManualClock {
    fn now(&self) -> Result<JobTimestamp, RuntimeClockError> {
        JobTimestamp::new(self.seconds.load(Ordering::SeqCst))
            .map_err(|_| RuntimeClockError::Overflow)
    }
}

#[derive(Clone)]
struct ImmediateHandler {
    result: JobHandlerResult,
}

impl ImmediateHandler {
    fn succeeds() -> Self {
        Self {
            result: JobHandlerResult::Succeeded,
        }
    }

    fn fails() -> Self {
        Self {
            result: JobHandlerResult::Failed(gamepulse_application::JobHandlerFailure::new(
                "deterministic handler failure",
            )),
        }
    }
}

impl JobHandler for ImmediateHandler {
    fn job_type(&self) -> RuntimeJobType {
        RuntimeJobType::SourceHourlyDiscovery
    }

    fn handle(&self, _job: TypedJob) -> JobHandlerFuture {
        let result = self.result.clone();
        Box::pin(async move { result })
    }
}

struct GateHandler {
    releases: Mutex<VecDeque<oneshot::Receiver<()>>>,
    started: Arc<AtomicUsize>,
    start_notifications: Arc<Notify>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
}

impl GateHandler {
    fn new(releases: Vec<oneshot::Receiver<()>>) -> Self {
        Self {
            releases: Mutex::new(releases.into()),
            started: Arc::new(AtomicUsize::new(0)),
            start_notifications: Arc::new(Notify::new()),
            active: Arc::new(AtomicUsize::new(0)),
            max_active: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn started(&self) -> usize {
        self.started.load(Ordering::SeqCst)
    }

    fn start_notifications(&self) -> Arc<Notify> {
        Arc::clone(&self.start_notifications)
    }

    fn max_active(&self) -> usize {
        self.max_active.load(Ordering::SeqCst)
    }
}

impl JobHandler for GateHandler {
    fn job_type(&self) -> RuntimeJobType {
        RuntimeJobType::SourceHourlyDiscovery
    }

    fn handle(&self, _job: TypedJob) -> JobHandlerFuture {
        let release = self
            .releases
            .lock()
            .expect("test handler release queue must not be poisoned")
            .pop_front()
            .expect("test handler must have one release signal per started task");
        let started = Arc::clone(&self.started);
        let start_notifications = Arc::clone(&self.start_notifications);
        let active = Arc::clone(&self.active);
        let max_active = Arc::clone(&self.max_active);
        Box::pin(async move {
            started.fetch_add(1, Ordering::SeqCst);
            start_notifications.notify_one();
            let current = active.fetch_add(1, Ordering::SeqCst) + 1;
            let mut observed = max_active.load(Ordering::SeqCst);
            while observed < current {
                match max_active.compare_exchange_weak(
                    observed,
                    current,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => break,
                    Err(actual) => observed = actual,
                }
            }
            let _ = release.await;
            active.fetch_sub(1, Ordering::SeqCst);
            JobHandlerResult::Succeeded
        })
    }
}

fn timestamp(seconds: i64) -> JobTimestamp {
    JobTimestamp::new(seconds).expect("test timestamp must be valid")
}

fn store() -> Arc<Mutex<SqliteJobStore>> {
    Arc::new(Mutex::new(
        SqliteJobStore::open_in_memory().expect("test store must open"),
    ))
}

fn config(max_attempts: u32, concurrency_limit: usize, lease_seconds: i64) -> RuntimeConfig {
    RuntimeConfig::new(
        "runtime-test-worker",
        lease_seconds,
        concurrency_limit,
        HourlyJobSchedule::new(RuntimeJobType::SourceHourlyDiscovery, max_attempts)
            .expect("test schedule must be valid"),
    )
    .expect("test runtime config must be valid")
}

fn registry(handler: Arc<dyn JobHandler>) -> Arc<JobHandlerRegistry> {
    Arc::new(JobHandlerRegistry::new([handler]).expect("test handler registry must be valid"))
}

fn enqueue(store: &Arc<Mutex<SqliteJobStore>>, identity: &str, job_type: &str, max_attempts: u32) {
    let result = store
        .lock()
        .expect("test store must not be poisoned")
        .enqueue(
            JobRequest::new(
                identity,
                job_type,
                "opaque-test-work",
                max_attempts,
                timestamp(0),
            )
            .expect("test job request must be valid"),
        )
        .expect("test job must enqueue");
    assert!(matches!(result, JobEnqueueResult::Enqueued(_)));
}

#[tokio::test]
async fn hourly_tick_is_durable_and_same_slot_is_deduplicated() {
    let store = store();
    let clock = Arc::new(ManualClock::new(3_605));
    let handler: Arc<dyn JobHandler> = Arc::new(ImmediateHandler::succeeds());
    let mut runtime = Runtime::new(
        store.clone(),
        clock.clone(),
        config(2, 1, 30),
        registry(handler),
    );

    assert_eq!(
        runtime.schedule_hourly().expect("first tick must schedule"),
        SchedulerOutcome::Enqueued
    );
    clock.set(7_199);
    assert_eq!(
        runtime.schedule_hourly().expect("rerun must schedule"),
        SchedulerOutcome::Duplicate
    );

    let job = store
        .lock()
        .expect("test store must not be poisoned")
        .job("hourly:source.hourly-discovery:1")
        .expect("job lookup must succeed")
        .expect("durable hourly job must exist");
    assert_eq!(job.status(), JobStatus::Ready);
    assert_eq!(job.attempt_count(), 0);
}

#[tokio::test]
async fn typed_handler_success_completes_the_claimed_durable_job() {
    let store = store();
    let clock = Arc::new(ManualClock::new(7_200));
    let handler: Arc<dyn JobHandler> = Arc::new(ImmediateHandler::succeeds());
    let mut runtime = Runtime::new(store.clone(), clock, config(2, 1, 30), registry(handler));
    runtime.schedule_hourly().expect("job must schedule");

    assert_eq!(
        runtime
            .dispatch_available()
            .expect("dispatch must succeed")
            .claimed,
        1
    );
    let settled = runtime.join_all().await.expect("task must join");
    assert_eq!(settled.settled, [RuntimeTaskOutcome::Succeeded]);

    let mut store = store.lock().expect("test store must not be poisoned");
    let job = store
        .job("hourly:source.hourly-discovery:2")
        .expect("job lookup must succeed")
        .expect("job must exist");
    assert_eq!(job.status(), JobStatus::Succeeded);
    let attempts = store
        .attempts(job.identity())
        .expect("attempt lookup must succeed");
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].claim_token(), 1);
    assert_eq!(attempts[0].outcome(), JobAttemptOutcome::Succeeded);
}

#[tokio::test]
async fn handler_failure_uses_the_existing_retry_and_terminal_path() {
    let store = store();
    let clock = Arc::new(ManualClock::new(7_200));
    let handler: Arc<dyn JobHandler> = Arc::new(ImmediateHandler::fails());
    let mut runtime = Runtime::new(store.clone(), clock, config(2, 1, 30), registry(handler));
    runtime.schedule_hourly().expect("job must schedule");

    runtime
        .dispatch_available()
        .expect("first dispatch must succeed");
    let first = runtime.join_all().await.expect("first task must join");
    assert_eq!(
        first.settled,
        [RuntimeTaskOutcome::Failed(JobFailureResult::ReadyForRetry)]
    );

    runtime
        .dispatch_available()
        .expect("retry dispatch must succeed");
    let second = runtime.join_all().await.expect("second task must join");
    assert_eq!(
        second.settled,
        [RuntimeTaskOutcome::Failed(JobFailureResult::Failed)]
    );

    let mut store = store.lock().expect("test store must not be poisoned");
    let job = store
        .job("hourly:source.hourly-discovery:2")
        .expect("job lookup must succeed")
        .expect("job must exist");
    assert_eq!(job.status(), JobStatus::Failed);
    assert_eq!(job.attempt_count(), 2);
    let attempts = store
        .attempts(job.identity())
        .expect("attempt lookup must succeed");
    assert_eq!(attempts[0].outcome(), JobAttemptOutcome::RetryableFailure);
    assert_eq!(attempts[1].outcome(), JobAttemptOutcome::TerminalFailure);
}

#[tokio::test]
async fn expired_claim_recovery_cannot_let_stale_completion_win() {
    let store = store();
    enqueue(&store, "expired-claim", "source.hourly-discovery", 2);
    let clock = Arc::new(ManualClock::new(0));
    let (release, receiver) = oneshot::channel();
    let first_handler = Arc::new(GateHandler::new(vec![receiver]));
    let first_handler_port: Arc<dyn JobHandler> = first_handler.clone();
    let mut first_runtime = Runtime::new(
        store.clone(),
        clock.clone(),
        config(2, 1, 1),
        registry(first_handler_port),
    );
    first_runtime
        .dispatch_available()
        .expect("first dispatch must claim the job");
    tokio::task::yield_now().await;
    assert_eq!(first_handler.started(), 1);

    clock.set(2);
    let second_handler: Arc<dyn JobHandler> = Arc::new(ImmediateHandler::succeeds());
    let mut recovering_runtime = Runtime::new(
        store.clone(),
        clock.clone(),
        config(2, 1, 1),
        registry(second_handler),
    );
    recovering_runtime
        .dispatch_available()
        .expect("recovery dispatch must claim the recovered job");
    assert_eq!(
        recovering_runtime
            .join_all()
            .await
            .expect("recovery task must join")
            .settled,
        [RuntimeTaskOutcome::Succeeded]
    );

    release
        .send(())
        .expect("blocked handler must still be waiting");
    assert_eq!(
        first_runtime
            .join_all()
            .await
            .expect("stale task must join")
            .settled,
        [RuntimeTaskOutcome::CompletionRejected]
    );

    let mut store = store.lock().expect("test store must not be poisoned");
    let job = store
        .job("expired-claim")
        .expect("job lookup must succeed")
        .expect("job must exist");
    assert_eq!(job.status(), JobStatus::Succeeded);
    let attempts = store
        .attempts("expired-claim")
        .expect("attempt lookup must succeed");
    assert_eq!(
        attempts
            .iter()
            .map(|attempt| attempt.outcome())
            .collect::<Vec<_>>(),
        [JobAttemptOutcome::Expired, JobAttemptOutcome::Succeeded]
    );
}

#[tokio::test]
async fn configured_concurrency_is_never_exceeded() {
    let store = store();
    enqueue(&store, "concurrency-1", "source.hourly-discovery", 1);
    enqueue(&store, "concurrency-2", "source.hourly-discovery", 1);
    enqueue(&store, "concurrency-3", "source.hourly-discovery", 1);
    let clock = Arc::new(ManualClock::new(0));
    let (release_one, receiver_one) = oneshot::channel();
    let (release_two, receiver_two) = oneshot::channel();
    let (release_three, receiver_three) = oneshot::channel();
    let gate_handler = Arc::new(GateHandler::new(vec![
        receiver_one,
        receiver_two,
        receiver_three,
    ]));
    let handler: Arc<dyn JobHandler> = gate_handler.clone();
    let mut runtime = Runtime::new(store, clock, config(1, 2, 30), registry(handler));

    assert_eq!(
        runtime
            .dispatch_available()
            .expect("dispatch must succeed")
            .claimed,
        2
    );
    tokio::task::yield_now().await;
    assert_eq!(gate_handler.started(), 2);
    assert_eq!(
        runtime
            .dispatch_available()
            .expect("capacity check must succeed")
            .claimed,
        0
    );

    release_one.send(()).expect("first task must be waiting");
    release_two.send(()).expect("second task must be waiting");
    runtime.join_all().await.expect("first two tasks must join");

    assert_eq!(
        runtime
            .dispatch_available()
            .expect("third dispatch must succeed")
            .claimed,
        1
    );
    tokio::task::yield_now().await;
    assert_eq!(gate_handler.started(), 3);
    release_three.send(()).expect("third task must be waiting");
    runtime.join_all().await.expect("third task must join");
    assert!(gate_handler.max_active() <= 2);
}

#[tokio::test]
async fn unsupported_and_failing_handler_routing_cannot_report_source_discovery_success() {
    let store = store();
    enqueue(&store, "unsupported-job", "unsupported.job-type", 1);
    let clock = Arc::new(ManualClock::new(7_200));
    let failing_handler: Arc<dyn JobHandler> = Arc::new(ImmediateHandler::fails());
    let mut runtime = Runtime::new(
        store.clone(),
        clock,
        config(1, 1, 30),
        registry(failing_handler),
    );
    runtime.schedule_hourly().expect("job must schedule");

    runtime.dispatch_available().expect("dispatch must succeed");
    assert_eq!(
        runtime
            .join_all()
            .await
            .expect("failing handler task must join")
            .settled,
        [RuntimeTaskOutcome::Failed(JobFailureResult::Failed)]
    );
    runtime.dispatch_available().expect("dispatch must succeed");
    assert_eq!(
        runtime
            .join_all()
            .await
            .expect("failing handler task must join")
            .settled,
        [RuntimeTaskOutcome::Failed(JobFailureResult::Failed)]
    );

    let mut store = store.lock().expect("test store must not be poisoned");
    assert_eq!(
        store
            .job("unsupported-job")
            .expect("job lookup must succeed")
            .expect("unsupported job must exist")
            .status(),
        JobStatus::Failed
    );
    let job = store
        .job("hourly:source.hourly-discovery:2")
        .expect("job lookup must succeed")
        .expect("job must exist");
    assert_eq!(job.status(), JobStatus::Failed);
}

#[tokio::test]
async fn production_loop_refills_capacity_after_task_completion_without_an_hourly_wait() {
    let store = store();
    enqueue(&store, "a-first-ready", "source.hourly-discovery", 1);
    enqueue(&store, "b-second-ready", "source.hourly-discovery", 1);
    let clock = Arc::new(ManualClock::new(0));
    let (release_first, first_receiver) = oneshot::channel();
    let (release_second, second_receiver) = oneshot::channel();
    let gate_handler = Arc::new(GateHandler::new(vec![first_receiver, second_receiver]));
    let handler: Arc<dyn JobHandler> = gate_handler.clone();
    let runtime = Runtime::new(store.clone(), clock, config(1, 1, 30), registry(handler));
    let (shutdown, shutdown_signal) = oneshot::channel();
    let starts = gate_handler.start_notifications();
    let first_started = starts.notified();
    let run = tokio::spawn(async move {
        let mut runtime = runtime;
        runtime
            .run_until_shutdown(async move {
                let _ = shutdown_signal.await;
            })
            .await
            .expect("production loop must stop cleanly");
        runtime
    });

    first_started.await;
    assert_eq!(gate_handler.started(), 1);
    let second_started = starts.notified();
    release_first
        .send(())
        .expect("first task must still be waiting");
    second_started.await;
    assert_eq!(gate_handler.started(), 2);

    shutdown
        .send(())
        .expect("production loop must still be running");
    release_second
        .send(())
        .expect("second task must still be waiting");
    let _runtime = run.await.expect("production loop task must join");

    let mut store = store.lock().expect("test store must not be poisoned");
    assert_eq!(
        store
            .job("a-first-ready")
            .expect("job lookup must succeed")
            .expect("first job must exist")
            .status(),
        JobStatus::Succeeded
    );
    assert_eq!(
        store
            .job("b-second-ready")
            .expect("job lookup must succeed")
            .expect("second job must exist")
            .status(),
        JobStatus::Succeeded
    );
    assert_eq!(
        store
            .job("hourly:source.hourly-discovery:0")
            .expect("job lookup must succeed")
            .expect("initial timer job must exist")
            .status(),
        JobStatus::Ready
    );
}

#[tokio::test]
async fn pre_resolved_shutdown_prevents_production_loop_scheduling_and_claiming() {
    let store = store();
    enqueue(
        &store,
        "ready-before-shutdown",
        "source.hourly-discovery",
        1,
    );
    let clock = Arc::new(ManualClock::new(0));
    let handler: Arc<dyn JobHandler> = Arc::new(ImmediateHandler::succeeds());
    let mut runtime = Runtime::new(store.clone(), clock, config(1, 1, 30), registry(handler));
    let (shutdown, shutdown_signal) = oneshot::channel();
    shutdown
        .send(())
        .expect("pre-resolved shutdown signal must send");

    runtime
        .run_until_shutdown(async move {
            let _ = shutdown_signal.await;
        })
        .await
        .expect("pre-resolved shutdown must stop cleanly");

    let mut store = store.lock().expect("test store must not be poisoned");
    let ready = store
        .job("ready-before-shutdown")
        .expect("job lookup must succeed")
        .expect("ready job must exist");
    assert_eq!(ready.status(), JobStatus::Ready);
    assert_eq!(ready.attempt_count(), 0);
    assert!(
        store
            .job("hourly:source.hourly-discovery:0")
            .expect("job lookup must succeed")
            .is_none()
    );
}

#[tokio::test]
async fn graceful_shutdown_stops_new_schedule_and_claims_then_joins_started_work() {
    let store = store();
    enqueue(
        &store,
        "a-started-before-shutdown",
        "source.hourly-discovery",
        1,
    );
    enqueue(
        &store,
        "z-ready-after-shutdown",
        "source.hourly-discovery",
        1,
    );
    let clock = Arc::new(ManualClock::new(0));
    let (release, receiver) = oneshot::channel();
    let gate_handler = Arc::new(GateHandler::new(vec![receiver]));
    let handler: Arc<dyn JobHandler> = gate_handler.clone();
    let mut runtime = Runtime::new(store.clone(), clock, config(1, 1, 30), registry(handler));

    assert_eq!(
        runtime
            .dispatch_available()
            .expect("dispatch must succeed")
            .claimed,
        1
    );
    tokio::task::yield_now().await;
    assert_eq!(gate_handler.started(), 1);
    runtime.begin_shutdown();
    assert_eq!(
        runtime
            .schedule_hourly()
            .expect("stopped scheduler must succeed"),
        SchedulerOutcome::Stopped
    );
    assert_eq!(
        runtime
            .dispatch_available()
            .expect("stopped dispatcher must succeed")
            .claimed,
        0
    );

    release
        .send(())
        .expect("started task must still be waiting");
    assert_eq!(
        runtime
            .shutdown()
            .await
            .expect("shutdown must join tasks")
            .settled,
        [RuntimeTaskOutcome::Succeeded]
    );
    let mut store = store.lock().expect("test store must not be poisoned");
    assert_eq!(
        store
            .job("a-started-before-shutdown")
            .expect("job lookup must succeed")
            .expect("started job must exist")
            .status(),
        JobStatus::Succeeded
    );
    assert_eq!(
        store
            .job("z-ready-after-shutdown")
            .expect("job lookup must succeed")
            .expect("unclaimed job must exist")
            .status(),
        JobStatus::Ready
    );
    assert!(
        store
            .job("hourly:source.hourly-discovery:0")
            .expect("job lookup must succeed")
            .is_none()
    );
}
