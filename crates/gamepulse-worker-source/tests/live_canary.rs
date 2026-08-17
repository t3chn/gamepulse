#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::process::Command;

use gamepulse_worker_source::{
    ListMode, ReviewKind, SourceError, parse_listing_page, parse_review_page,
};
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue};
use reqwest::{Client, StatusCode, Url, redirect};
use serde::Serialize;
use serde_json::Value;

const BACKEND_BASE_URL: &str = "https://backend.metacritic.com/";
const REQUEST_TIMEOUT_SECONDS: u64 = 20;
const MAX_RESPONSE_BYTES: usize = 1_048_576;
const NEW_RELEASES_LIMIT: u32 = 20;
const REVIEW_LIMIT: u32 = 20;
const LIVE_OPT_IN: &str = "GAMEPULSE_M028_LIVE_DIAGNOSTIC";
const DIAGNOSTIC_SCHEMA_VERSION: &str = "gamepulse.diagnostic.v1";

const LISTING: &str = include_str!("fixtures/listing-page.json");
const M011_CRITIC: &str = include_str!("fixtures/m011-critic-review-page.json");
const M011_USER: &str = include_str!("fixtures/m011-user-review-page.json");
const M015_CRITIC_CLAMP: &str = include_str!("fixtures/m015-critic-server-clamp-page.json");
const M017_REVIEW_TERMINAL_EMPTY: &str = include_str!("fixtures/m017-review-terminal-empty.json");

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DiagnosticMode {
    Fixture,
    Finder,
    ReviewContinuation,
}

impl DiagnosticMode {
    const fn ceiling(self) -> u8 {
        match self {
            Self::Fixture | Self::ReviewContinuation => 3,
            Self::Finder => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum TerminalVerdict {
    FixtureValidated,
    ContractReady,
    AccessDenied,
    RateLimited,
    SourceRejected,
    NoCandidate,
    RequestBudgetExhausted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum StatusCategory {
    Ok,
    Forbidden,
    RateLimited,
    Other,
    NotAttempted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ContinuationPresence {
    Missing,
    Null,
    Object,
    Other,
    NotChecked,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum HrefPresence {
    Missing,
    Null,
    String,
    Other,
    NotApplicable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ParserOutcome {
    Accepted,
    Rejected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ExchangeKind {
    Finder,
    CriticReview,
    UserReview,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
struct LinkChecks {
    scheme: bool,
    host: bool,
    path: bool,
    query: bool,
    progression: bool,
    limit: bool,
    total_boundary: bool,
}

impl LinkChecks {
    const fn unchecked() -> Self {
        Self {
            scheme: false,
            host: false,
            path: false,
            query: false,
            progression: false,
            limit: false,
            total_boundary: false,
        }
    }

    const fn all_false(self) -> bool {
        !self.scheme
            && !self.host
            && !self.path
            && !self.query
            && !self.progression
            && !self.limit
            && !self.total_boundary
    }

    const fn all_true(self) -> bool {
        self.scheme
            && self.host
            && self.path
            && self.query
            && self.progression
            && self.limit
            && self.total_boundary
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ExchangeReport {
    request: ExchangeKind,
    status_category: StatusCategory,
    expected_content_type: bool,
    utf8: bool,
    json: bool,
    item_count: u64,
    numeric_total: bool,
    continuation_presence: ContinuationPresence,
    href_presence: HrefPresence,
    link_checks: LinkChecks,
    parser: ParserOutcome,
    safe_category: SafeCategory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum SafeCategory {
    ReviewContinuationLink,
    OtherMandatoryStage,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct DiagnosticReport {
    schema_version: &'static str,
    mode: DiagnosticMode,
    request_count: u8,
    request_ceiling: u8,
    terminal_verdict: TerminalVerdict,
    exchanges: Vec<ExchangeReport>,
}

impl DiagnosticReport {
    fn render(&self) -> String {
        assert!(
            self.is_schema_valid(),
            "aggregate diagnostic report must satisfy schema v1"
        );
        serde_json::to_string(self).expect("aggregate diagnostic report must serialize")
    }

    fn is_schema_valid(&self) -> bool {
        let expected_requests = [
            ExchangeKind::Finder,
            ExchangeKind::CriticReview,
            ExchangeKind::UserReview,
        ];
        let accepted = self
            .exchanges
            .iter()
            .all(|exchange| exchange.parser == ParserOutcome::Accepted);
        let prior_exchanges_accepted = self
            .exchanges
            .get(..self.exchanges.len().saturating_sub(1))
            .is_some_and(|exchanges| {
                exchanges
                    .iter()
                    .all(|exchange| exchange.parser == ParserOutcome::Accepted)
            });
        let terminal_valid = match self.terminal_verdict {
            TerminalVerdict::FixtureValidated => {
                self.mode == DiagnosticMode::Fixture
                    && self.request_count == 3
                    && accepted
                    && self
                        .exchanges
                        .first()
                        .is_some_and(|exchange| exchange.item_count > 0)
            }
            TerminalVerdict::ContractReady => {
                self.mode != DiagnosticMode::Fixture
                    && self.request_count == self.mode.ceiling()
                    && accepted
                    && self
                        .exchanges
                        .first()
                        .is_some_and(|exchange| exchange.item_count > 0)
            }
            TerminalVerdict::AccessDenied => self.exchanges.last().is_some_and(|exchange| {
                prior_exchanges_accepted
                    && exchange.status_category == StatusCategory::Forbidden
                    && exchange.parser == ParserOutcome::Rejected
            }),
            TerminalVerdict::RateLimited => self.exchanges.last().is_some_and(|exchange| {
                prior_exchanges_accepted
                    && exchange.status_category == StatusCategory::RateLimited
                    && exchange.parser == ParserOutcome::Rejected
            }),
            TerminalVerdict::SourceRejected => self.exchanges.last().is_some_and(|exchange| {
                prior_exchanges_accepted
                    && self.exchanges.iter().all(|exchange| {
                        exchange.status_category != StatusCategory::Forbidden
                            && exchange.status_category != StatusCategory::RateLimited
                    })
                    && exchange.parser == ParserOutcome::Rejected
            }),
            TerminalVerdict::NoCandidate => {
                self.request_count == 1
                    && self.exchanges.len() == 1
                    && self.exchanges[0].request == ExchangeKind::Finder
                    && self.exchanges[0].parser == ParserOutcome::Accepted
                    && self.exchanges[0].item_count == 0
            }
            TerminalVerdict::RequestBudgetExhausted => {
                self.request_count == self.request_ceiling && accepted
            }
        };

        self.schema_version == DIAGNOSTIC_SCHEMA_VERSION
            && self.request_ceiling == self.mode.ceiling()
            && self.request_count > 0
            && self.request_count <= self.request_ceiling
            && usize::from(self.request_count) == self.exchanges.len()
            && self
                .exchanges
                .iter()
                .zip(expected_requests)
                .all(|(exchange, expected)| {
                    exchange.request == expected && exchange.is_schema_valid()
                })
            && terminal_valid
    }
}

impl ExchangeReport {
    fn is_schema_valid(&self) -> bool {
        let is_review = self.request != ExchangeKind::Finder;
        let presence_valid_for_rejected = match (self.continuation_presence, self.href_presence) {
            (ContinuationPresence::NotChecked, HrefPresence::NotApplicable)
            | (ContinuationPresence::Missing, HrefPresence::NotApplicable)
            | (ContinuationPresence::Null, HrefPresence::NotApplicable)
            | (ContinuationPresence::Other, HrefPresence::NotApplicable) => {
                self.link_checks.all_false()
            }
            (
                ContinuationPresence::Object,
                HrefPresence::Missing | HrefPresence::Null | HrefPresence::Other,
            ) => self.link_checks.all_false(),
            (ContinuationPresence::Object, HrefPresence::String) => true,
            _ => false,
        };

        match self.parser {
            ParserOutcome::Accepted => {
                self.status_category == StatusCategory::Ok
                    && self.expected_content_type
                    && self.utf8
                    && self.json
                    && self.numeric_total
                    && self.safe_category == SafeCategory::OtherMandatoryStage
                    && match (self.continuation_presence, self.href_presence) {
                        (ContinuationPresence::Missing, HrefPresence::NotApplicable) => {
                            self.link_checks.all_false()
                        }
                        (ContinuationPresence::Object, HrefPresence::Missing) => {
                            is_review && self.link_checks.all_false()
                        }
                        (ContinuationPresence::Object, HrefPresence::String) => {
                            self.link_checks.all_true()
                        }
                        _ => false,
                    }
            }
            ParserOutcome::Rejected => {
                self.status_category != StatusCategory::NotAttempted
                    && if self.status_category != StatusCategory::Ok {
                        !self.utf8
                            && !self.json
                            && !self.numeric_total
                            && self.item_count == 0
                            && self.continuation_presence == ContinuationPresence::NotChecked
                            && self.href_presence == HrefPresence::NotApplicable
                            && self.link_checks.all_false()
                            && self.safe_category == SafeCategory::OtherMandatoryStage
                    } else {
                        let structural_stage_valid = match self.continuation_presence {
                            ContinuationPresence::NotChecked => {
                                !self.json && !self.numeric_total && self.item_count == 0
                            }
                            ContinuationPresence::Missing
                            | ContinuationPresence::Null
                            | ContinuationPresence::Object
                            | ContinuationPresence::Other => {
                                self.expected_content_type && self.utf8 && self.json
                            }
                        };
                        structural_stage_valid
                            && presence_valid_for_rejected
                            && match self.safe_category {
                                SafeCategory::OtherMandatoryStage => true,
                                SafeCategory::ReviewContinuationLink => {
                                    is_review
                                        && self.continuation_presence
                                            == ContinuationPresence::Object
                                        && self.expected_content_type
                                        && self.utf8
                                        && self.json
                                        && self.numeric_total
                                }
                            }
                    }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum ProbeRequest<'a> {
    Finder,
    Review {
        kind: ReviewKind,
        candidate_slug: &'a str,
    },
}

impl ProbeRequest<'_> {
    const fn exchange_kind(self) -> ExchangeKind {
        match self {
            Self::Finder => ExchangeKind::Finder,
            Self::Review {
                kind: ReviewKind::Critic,
                ..
            } => ExchangeKind::CriticReview,
            Self::Review {
                kind: ReviewKind::User,
                ..
            } => ExchangeKind::UserReview,
        }
    }

    const fn requested_limit(self) -> u32 {
        match self {
            Self::Finder => NEW_RELEASES_LIMIT,
            Self::Review { .. } => REVIEW_LIMIT,
        }
    }

    const fn kind(self) -> Option<ReviewKind> {
        match self {
            Self::Finder => None,
            Self::Review { kind, .. } => Some(kind),
        }
    }
}

#[derive(Clone)]
struct DiagnosticResponse {
    status: u16,
    content_type_is_json: bool,
    body: Vec<u8>,
}

impl DiagnosticResponse {
    fn fixture(status: u16, content_type_is_json: bool, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            content_type_is_json,
            body: body.into(),
        }
    }
}

enum DiagnosticTransport {
    Fixture {
        responses: VecDeque<Result<DiagnosticResponse, ()>>,
        calls: u8,
    },
    Live {
        client: Client,
    },
}

impl DiagnosticTransport {
    fn fixture(responses: impl IntoIterator<Item = Result<DiagnosticResponse, ()>>) -> Self {
        Self::Fixture {
            responses: responses.into_iter().collect(),
            calls: 0,
        }
    }

    fn live() -> Result<Self, ()> {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        let client = Client::builder()
            .default_headers(headers)
            .redirect(redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECONDS))
            .build()
            .map_err(|_| ())?;
        Ok(Self::Live { client })
    }

    async fn fetch(&mut self, request: ProbeRequest<'_>) -> Result<DiagnosticResponse, ()> {
        match self {
            Self::Fixture { responses, calls } => {
                *calls = calls.checked_add(1).ok_or(())?;
                responses.pop_front().ok_or(())?
            }
            Self::Live { client } => fetch_live_response(client, request).await,
        }
    }

    fn fixture_calls(&self) -> u8 {
        match self {
            Self::Fixture { calls, .. } => *calls,
            Self::Live { .. } => 0,
        }
    }
}

struct AttemptBudget {
    ceiling: u8,
    attempts: u8,
}

impl AttemptBudget {
    const fn new(ceiling: u8) -> Self {
        Self {
            ceiling,
            attempts: 0,
        }
    }

    fn reserve(&mut self) -> bool {
        if self.attempts >= self.ceiling {
            return false;
        }
        self.attempts += 1;
        true
    }
}

struct DiagnosticCanary {
    mode: DiagnosticMode,
    budget: AttemptBudget,
    transport: DiagnosticTransport,
}

struct Execution {
    report: ExchangeReport,
    candidate_slug: Option<String>,
    budget_exhausted: bool,
}

impl DiagnosticCanary {
    fn fixture(
        mode: DiagnosticMode,
        responses: impl IntoIterator<Item = Result<DiagnosticResponse, ()>>,
    ) -> Self {
        Self {
            mode,
            budget: AttemptBudget::new(mode.ceiling()),
            transport: DiagnosticTransport::fixture(responses),
        }
    }

    fn live(mode: DiagnosticMode) -> Result<Self, ()> {
        Ok(Self {
            mode,
            budget: AttemptBudget::new(mode.ceiling()),
            transport: DiagnosticTransport::live()?,
        })
    }

    async fn run(mut self) -> DiagnosticReport {
        let finder = self.execute(ProbeRequest::Finder).await;
        let mut exchanges = vec![finder.report];
        let mut budget_exhausted = finder.budget_exhausted;
        let candidate_slug = finder.candidate_slug;

        if self.mode == DiagnosticMode::Finder || budget_exhausted || !is_accepted(&exchanges) {
            return self.finish(exchanges, candidate_slug.is_some(), budget_exhausted);
        }

        let Some(candidate_slug) = candidate_slug else {
            return self.finish(exchanges, false, budget_exhausted);
        };

        for kind in [ReviewKind::Critic, ReviewKind::User] {
            let execution = self
                .execute(ProbeRequest::Review {
                    kind,
                    candidate_slug: &candidate_slug,
                })
                .await;
            budget_exhausted |= execution.budget_exhausted;
            exchanges.push(execution.report);
            if budget_exhausted || !is_accepted(&exchanges) {
                break;
            }
        }

        self.finish(exchanges, true, budget_exhausted)
    }

    async fn execute(&mut self, request: ProbeRequest<'_>) -> Execution {
        if !self.budget.reserve() {
            return Execution {
                report: skipped_report(request),
                candidate_slug: None,
                budget_exhausted: true,
            };
        }

        let response = match self.transport.fetch(request).await {
            Ok(response) => response,
            Err(()) => {
                return Execution {
                    report: rejected_transport_report(request),
                    candidate_slug: None,
                    budget_exhausted: false,
                };
            }
        };
        inspect_response(request, response)
    }

    fn finish(
        self,
        exchanges: Vec<ExchangeReport>,
        has_candidate: bool,
        budget_exhausted: bool,
    ) -> DiagnosticReport {
        let terminal_verdict = if budget_exhausted {
            TerminalVerdict::RequestBudgetExhausted
        } else if exchanges
            .iter()
            .any(|report| report.status_category == StatusCategory::Forbidden)
        {
            TerminalVerdict::AccessDenied
        } else if exchanges
            .iter()
            .any(|report| report.status_category == StatusCategory::RateLimited)
        {
            TerminalVerdict::RateLimited
        } else if !is_accepted(&exchanges) {
            TerminalVerdict::SourceRejected
        } else if !has_candidate {
            TerminalVerdict::NoCandidate
        } else if self.mode == DiagnosticMode::Fixture {
            TerminalVerdict::FixtureValidated
        } else {
            TerminalVerdict::ContractReady
        };

        DiagnosticReport {
            schema_version: DIAGNOSTIC_SCHEMA_VERSION,
            mode: self.mode,
            request_count: self.budget.attempts,
            request_ceiling: self.budget.ceiling,
            terminal_verdict,
            exchanges,
        }
    }
}

fn is_accepted(exchanges: &[ExchangeReport]) -> bool {
    exchanges
        .iter()
        .all(|report| report.parser == ParserOutcome::Accepted)
}

fn skipped_report(request: ProbeRequest<'_>) -> ExchangeReport {
    ExchangeReport {
        request: request.exchange_kind(),
        status_category: StatusCategory::NotAttempted,
        expected_content_type: false,
        utf8: false,
        json: false,
        item_count: 0,
        numeric_total: false,
        continuation_presence: ContinuationPresence::NotChecked,
        href_presence: HrefPresence::NotApplicable,
        link_checks: LinkChecks::unchecked(),
        parser: ParserOutcome::Rejected,
        safe_category: SafeCategory::OtherMandatoryStage,
    }
}

fn rejected_transport_report(request: ProbeRequest<'_>) -> ExchangeReport {
    ExchangeReport {
        request: request.exchange_kind(),
        status_category: StatusCategory::Other,
        expected_content_type: false,
        utf8: false,
        json: false,
        item_count: 0,
        numeric_total: false,
        continuation_presence: ContinuationPresence::NotChecked,
        href_presence: HrefPresence::NotApplicable,
        link_checks: LinkChecks::unchecked(),
        parser: ParserOutcome::Rejected,
        safe_category: SafeCategory::OtherMandatoryStage,
    }
}

fn inspect_response(request: ProbeRequest<'_>, response: DiagnosticResponse) -> Execution {
    let status_category = status_category(response.status);
    if status_category != StatusCategory::Ok || !response.content_type_is_json {
        return Execution {
            report: rejected_response_report(
                request,
                status_category,
                response.content_type_is_json,
            ),
            candidate_slug: None,
            budget_exhausted: false,
        };
    }

    if response.body.len() > MAX_RESPONSE_BYTES {
        return Execution {
            report: rejected_response_report(request, status_category, true),
            candidate_slug: None,
            budget_exhausted: false,
        };
    }

    let Ok(body) = String::from_utf8(response.body) else {
        return Execution {
            report: rejected_response_report(request, status_category, true),
            candidate_slug: None,
            budget_exhausted: false,
        };
    };

    let shape = structural_shape(request, &body);
    let (parser, safe_category, candidate_slug) = match request {
        ProbeRequest::Finder => {
            match parse_listing_page(ListMode::NewReleases, 0, NEW_RELEASES_LIMIT, &body) {
                Ok(page) => (
                    ParserOutcome::Accepted,
                    SafeCategory::OtherMandatoryStage,
                    page.games.into_iter().next().map(|game| game.slug),
                ),
                Err(error) => (
                    ParserOutcome::Rejected,
                    safe_category_for(None, &error),
                    None,
                ),
            }
        }
        ProbeRequest::Review {
            kind,
            candidate_slug,
        } => match parse_review_page(kind, candidate_slug, 0, REVIEW_LIMIT, &body) {
            Ok(_) => (
                ParserOutcome::Accepted,
                SafeCategory::OtherMandatoryStage,
                None,
            ),
            Err(error) => (
                ParserOutcome::Rejected,
                safe_category_for(Some(kind), &error),
                None,
            ),
        },
    };

    Execution {
        report: ExchangeReport {
            request: request.exchange_kind(),
            status_category,
            expected_content_type: true,
            utf8: true,
            json: shape.json,
            item_count: shape.item_count,
            numeric_total: shape.numeric_total,
            continuation_presence: shape.continuation_presence,
            href_presence: shape.href_presence,
            link_checks: shape.link_checks,
            parser,
            safe_category,
        },
        candidate_slug,
        budget_exhausted: false,
    }
}

fn rejected_response_report(
    request: ProbeRequest<'_>,
    status_category: StatusCategory,
    expected_content_type: bool,
) -> ExchangeReport {
    ExchangeReport {
        request: request.exchange_kind(),
        status_category,
        expected_content_type,
        utf8: false,
        json: false,
        item_count: 0,
        numeric_total: false,
        continuation_presence: ContinuationPresence::NotChecked,
        href_presence: HrefPresence::NotApplicable,
        link_checks: LinkChecks::unchecked(),
        parser: ParserOutcome::Rejected,
        safe_category: SafeCategory::OtherMandatoryStage,
    }
}

fn safe_category_for(kind: Option<ReviewKind>, error: &SourceError) -> SafeCategory {
    match (kind, error) {
        (Some(_), SourceError::InvalidContinuation) => SafeCategory::ReviewContinuationLink,
        _ => SafeCategory::OtherMandatoryStage,
    }
}

fn status_category(status: u16) -> StatusCategory {
    match status {
        200 => StatusCategory::Ok,
        403 => StatusCategory::Forbidden,
        429 => StatusCategory::RateLimited,
        _ => StatusCategory::Other,
    }
}

fn is_application_json(content_type: &str) -> bool {
    content_type
        .split_once(';')
        .map_or(content_type, |(media_type, _)| media_type)
        .trim()
        .eq_ignore_ascii_case("application/json")
}

struct StructuralShape {
    json: bool,
    item_count: u64,
    numeric_total: bool,
    continuation_presence: ContinuationPresence,
    href_presence: HrefPresence,
    link_checks: LinkChecks,
}

fn structural_shape(request: ProbeRequest<'_>, body: &str) -> StructuralShape {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return StructuralShape {
            json: false,
            item_count: 0,
            numeric_total: false,
            continuation_presence: ContinuationPresence::NotChecked,
            href_presence: HrefPresence::NotApplicable,
            link_checks: LinkChecks::unchecked(),
        };
    };

    let item_count = value
        .pointer("/data/items")
        .and_then(Value::as_array)
        .and_then(|items| u64::try_from(items.len()).ok())
        .unwrap_or(0);
    let numeric_total = value
        .pointer("/data/totalResults")
        .is_some_and(Value::is_number);
    let total_results = value.pointer("/data/totalResults").and_then(Value::as_u64);
    let next = value.pointer("/links/next");

    let (continuation_presence, href_presence, link_checks) = match next {
        None => (
            ContinuationPresence::Missing,
            HrefPresence::NotApplicable,
            LinkChecks::unchecked(),
        ),
        Some(Value::Null) => (
            ContinuationPresence::Null,
            HrefPresence::NotApplicable,
            LinkChecks::unchecked(),
        ),
        Some(Value::Object(next)) => match next.get("href") {
            None => (
                ContinuationPresence::Object,
                HrefPresence::Missing,
                LinkChecks::unchecked(),
            ),
            Some(Value::Null) => (
                ContinuationPresence::Object,
                HrefPresence::Null,
                LinkChecks::unchecked(),
            ),
            Some(Value::String(href)) => (
                ContinuationPresence::Object,
                HrefPresence::String,
                inspect_link(request, href, item_count, total_results),
            ),
            Some(_) => (
                ContinuationPresence::Object,
                HrefPresence::Other,
                LinkChecks::unchecked(),
            ),
        },
        Some(_) => (
            ContinuationPresence::Other,
            HrefPresence::NotApplicable,
            LinkChecks::unchecked(),
        ),
    };

    StructuralShape {
        json: true,
        item_count,
        numeric_total,
        continuation_presence,
        href_presence,
        link_checks,
    }
}

fn inspect_link(
    request: ProbeRequest<'_>,
    href: &str,
    item_count: u64,
    total_results: Option<u64>,
) -> LinkChecks {
    let Ok(url) = Url::parse(href) else {
        return LinkChecks::unchecked();
    };
    let scheme = url.scheme() == "https";
    let host = url.host_str() == Some("backend.metacritic.com") && url.port().is_none();
    let path = match request {
        ProbeRequest::Finder => url.path() == "/finder/metacritic/web",
        ProbeRequest::Review {
            kind,
            candidate_slug,
        } => {
            let kind_path = match kind {
                ReviewKind::Critic => "critic",
                ReviewKind::User => "user",
            };
            url.path() == format!("/reviews/metacritic/{kind_path}/games/{candidate_slug}/web")
        }
    };
    let (next_offset, next_limit, query) = continuation_query(&url);
    let requested_limit = request.requested_limit();
    let critic_clamp = matches!(request.kind(), Some(ReviewKind::Critic))
        && item_count > 0
        && item_count < u64::from(requested_limit)
        && u32::try_from(item_count)
            .ok()
            .is_some_and(|count| next_offset == Some(count) && next_limit == Some(count));
    let progression = next_offset == Some(requested_limit) || critic_clamp;
    let limit = next_limit == Some(requested_limit) || critic_clamp;
    let total_boundary = match (next_offset, total_results) {
        (Some(offset), Some(total)) => u64::from(offset) < total,
        _ => false,
    };

    LinkChecks {
        scheme,
        host,
        path,
        query,
        progression,
        limit,
        total_boundary,
    }
}

fn continuation_query(url: &Url) -> (Option<u32>, Option<u32>, bool) {
    let mut offset = None;
    let mut limit = None;
    let mut valid = true;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "offset" if offset.is_none() => offset = value.parse::<u32>().ok(),
            "limit" if limit.is_none() => limit = value.parse::<u32>().ok(),
            "offset" | "limit" => valid = false,
            _ => {}
        }
    }
    (offset, limit, valid && offset.is_some() && limit.is_some())
}

async fn fetch_live_response(
    client: &Client,
    request: ProbeRequest<'_>,
) -> Result<DiagnosticResponse, ()> {
    let url = diagnostic_url(request)?;
    if !is_exact_allowed_request(&url, request) {
        return Err(());
    }
    let mut response = client.get(url).send().await.map_err(|_| ())?;
    let status = response.status();
    let content_type_is_json = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(is_application_json);
    if status != StatusCode::OK || !content_type_is_json {
        return Ok(DiagnosticResponse::fixture(
            status.as_u16(),
            content_type_is_json,
            Vec::new(),
        ));
    }
    let content_length = response.content_length().ok_or(())?;
    let capacity = usize::try_from(content_length).map_err(|_| ())?;
    if capacity > MAX_RESPONSE_BYTES {
        return Err(());
    }
    let mut body = Vec::with_capacity(capacity);
    while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
        let next_length = body.len().checked_add(chunk.len()).ok_or(())?;
        if next_length > MAX_RESPONSE_BYTES {
            return Err(());
        }
        body.extend_from_slice(chunk.as_ref());
    }
    Ok(DiagnosticResponse::fixture(200, true, body))
}

fn diagnostic_url(request: ProbeRequest<'_>) -> Result<Url, ()> {
    let mut url = Url::parse(BACKEND_BASE_URL).map_err(|_| ())?;
    match request {
        ProbeRequest::Finder => {
            url.set_path("/finder/metacritic/web");
            append_query(
                &mut url,
                &[
                    ("componentName", "new-releases-carousel"),
                    ("componentDisplayName", "Newly Released"),
                    ("componentType", "ProductList"),
                    ("sortBy", "-releaseDate"),
                    ("metaScoreMin", "1"),
                    ("offset", "0"),
                    ("limit", "20"),
                    ("mcoTypeId", "13"),
                ],
            );
        }
        ProbeRequest::Review {
            kind,
            candidate_slug,
        } => {
            if candidate_slug.is_empty()
                || !candidate_slug
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            {
                return Err(());
            }
            let kind_path = match kind {
                ReviewKind::Critic => "critic",
                ReviewKind::User => "user",
            };
            url.set_path(&format!(
                "/reviews/metacritic/{kind_path}/games/{candidate_slug}/web"
            ));
            let query = match kind {
                ReviewKind::Critic => vec![
                    ("offset", "0"),
                    ("limit", "20"),
                    ("sort", "date"),
                    ("componentName", "latest-critic-reviews"),
                    ("componentDisplayName", "Latest Critic Reviews"),
                    ("componentType", "ReviewList"),
                ],
                ReviewKind::User => vec![
                    ("offset", "0"),
                    ("limit", "20"),
                    ("orderBy", "score"),
                    ("orderType", "desc"),
                    ("componentName", "top-user-reviews"),
                    ("componentDisplayName", "Top User Reviews"),
                    ("componentType", "ReviewList"),
                ],
            };
            append_query(&mut url, &query);
        }
    }
    Ok(url)
}

fn append_query(url: &mut Url, pairs: &[(&str, &str)]) {
    let mut query = url.query_pairs_mut();
    for (key, value) in pairs {
        query.append_pair(key, value);
    }
}

fn is_exact_allowed_request(url: &Url, request: ProbeRequest<'_>) -> bool {
    if url.scheme() != "https"
        || url.host_str() != Some("backend.metacritic.com")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let expected_path = match request {
        ProbeRequest::Finder => "/finder/metacritic/web".to_owned(),
        ProbeRequest::Review {
            kind,
            candidate_slug,
        } => {
            let kind_path = match kind {
                ReviewKind::Critic => "critic",
                ReviewKind::User => "user",
            };
            format!("/reviews/metacritic/{kind_path}/games/{candidate_slug}/web")
        }
    };
    if url.path() != expected_path {
        return false;
    }
    let expected = diagnostic_url(request)
        .ok()
        .map(|expected| sorted_query_pairs(&expected));
    expected.is_some_and(|expected| sorted_query_pairs(url) == expected)
}

fn sorted_query_pairs(url: &Url) -> Vec<(String, String)> {
    let mut pairs = url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    pairs.sort_unstable();
    pairs
}

fn fixture_response(body: &str) -> Result<DiagnosticResponse, ()> {
    Ok(DiagnosticResponse::fixture(
        200,
        true,
        body.as_bytes().to_vec(),
    ))
}

fn json_with_next(body: &str, next: Value) -> String {
    let mut value: Value = serde_json::from_str(body).expect("fixture must be JSON");
    value["links"]["next"] = next;
    serde_json::to_string(&value).expect("fixture must serialize")
}

fn json_without_next(body: &str) -> String {
    let mut value: Value = serde_json::from_str(body).expect("fixture must be JSON");
    value["links"]
        .as_object_mut()
        .expect("fixture links must be an object")
        .remove("next");
    serde_json::to_string(&value).expect("fixture must serialize")
}

fn valid_exchange(request: ExchangeKind, item_count: u64) -> ExchangeReport {
    ExchangeReport {
        request,
        status_category: StatusCategory::Ok,
        expected_content_type: true,
        utf8: true,
        json: true,
        item_count,
        numeric_total: true,
        continuation_presence: ContinuationPresence::Missing,
        href_presence: HrefPresence::NotApplicable,
        link_checks: LinkChecks::unchecked(),
        parser: ParserOutcome::Accepted,
        safe_category: SafeCategory::OtherMandatoryStage,
    }
}

fn report_with_terminal(
    mode: DiagnosticMode,
    terminal_verdict: TerminalVerdict,
) -> DiagnosticReport {
    let count = match terminal_verdict {
        TerminalVerdict::AccessDenied
        | TerminalVerdict::RateLimited
        | TerminalVerdict::SourceRejected
        | TerminalVerdict::NoCandidate => 1,
        TerminalVerdict::FixtureValidated
        | TerminalVerdict::ContractReady
        | TerminalVerdict::RequestBudgetExhausted => mode.ceiling(),
    };
    let mut exchanges = [
        valid_exchange(ExchangeKind::Finder, 1),
        valid_exchange(ExchangeKind::CriticReview, 1),
        valid_exchange(ExchangeKind::UserReview, 1),
    ]
    .into_iter()
    .take(usize::from(count))
    .collect::<Vec<_>>();

    match terminal_verdict {
        TerminalVerdict::AccessDenied => {
            exchanges[0].status_category = StatusCategory::Forbidden;
            exchanges[0].parser = ParserOutcome::Rejected;
            exchanges[0].utf8 = false;
            exchanges[0].json = false;
            exchanges[0].item_count = 0;
            exchanges[0].numeric_total = false;
            exchanges[0].continuation_presence = ContinuationPresence::NotChecked;
            exchanges[0].href_presence = HrefPresence::NotApplicable;
        }
        TerminalVerdict::RateLimited => {
            exchanges[0].status_category = StatusCategory::RateLimited;
            exchanges[0].parser = ParserOutcome::Rejected;
            exchanges[0].utf8 = false;
            exchanges[0].json = false;
            exchanges[0].item_count = 0;
            exchanges[0].numeric_total = false;
            exchanges[0].continuation_presence = ContinuationPresence::NotChecked;
            exchanges[0].href_presence = HrefPresence::NotApplicable;
        }
        TerminalVerdict::SourceRejected => {
            exchanges[0].status_category = StatusCategory::Other;
            exchanges[0].parser = ParserOutcome::Rejected;
            exchanges[0].utf8 = false;
            exchanges[0].json = false;
            exchanges[0].item_count = 0;
            exchanges[0].numeric_total = false;
            exchanges[0].continuation_presence = ContinuationPresence::NotChecked;
            exchanges[0].href_presence = HrefPresence::NotApplicable;
        }
        TerminalVerdict::NoCandidate => exchanges[0].item_count = 0,
        TerminalVerdict::FixtureValidated
        | TerminalVerdict::ContractReady
        | TerminalVerdict::RequestBudgetExhausted => {}
    }

    DiagnosticReport {
        schema_version: DIAGNOSTIC_SCHEMA_VERSION,
        mode,
        request_count: count,
        request_ceiling: mode.ceiling(),
        terminal_verdict,
        exchanges,
    }
}

fn cargo_harness_output(_test_name: &str, report: &str) -> String {
    format!(
        "\nrunning 1 test\n{report}\n.\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n\n"
    )
}

fn run_wrapper_with_cargo_output(mode: &str, cargo_stdout: &str) -> std::process::Output {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_TEMPORARY_ROOT: AtomicUsize = AtomicUsize::new(0);

    let repository_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("worker-source manifest must remain two directories below the repository root");
    let temporary_root = std::env::temp_dir().join(format!(
        "gamepulse-diagnostic-wrapper-{}-{}",
        std::process::id(),
        NEXT_TEMPORARY_ROOT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&temporary_root).expect("wrapper test directory must be created");
    let cargo_path = temporary_root.join("cargo");
    let output_path = temporary_root.join("cargo-output");
    std::fs::write(
        &cargo_path,
        "#!/usr/bin/env bash\ncat \"$GAMEPULSE_TEST_CARGO_OUTPUT\"\nexit \"${GAMEPULSE_TEST_CARGO_EXIT:-0}\"\n",
    )
    .expect("wrapper test cargo shim must be written");
    std::fs::set_permissions(&cargo_path, std::fs::Permissions::from_mode(0o700))
        .expect("wrapper test cargo shim must be executable");
    std::fs::write(&output_path, cargo_stdout).expect("wrapper test output must be written");

    let mut path_entries = vec![temporary_root.clone()];
    path_entries.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let output = Command::new("/bin/bash")
        .arg(repository_root.join("scripts/diagnostic_canary.sh"))
        .arg(mode)
        .current_dir(repository_root)
        .env(
            "PATH",
            std::env::join_paths(path_entries).expect("test PATH must be valid"),
        )
        .env("GAMEPULSE_TEST_CARGO_OUTPUT", &output_path)
        .env("GAMEPULSE_TEST_CARGO_EXIT", "0")
        .output()
        .expect("diagnostic wrapper must start");

    std::fs::remove_dir_all(&temporary_root).expect("wrapper test directory must be removed");
    output
}

fn run_wrapper_with_unusable_tmpdir(mode: &str) -> std::process::Output {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_TEMPORARY_ROOT: AtomicUsize = AtomicUsize::new(0);

    let repository_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("worker-source manifest must remain two directories below the repository root");
    let temporary_root = std::env::temp_dir().join(format!(
        "gamepulse-diagnostic-wrapper-tmpdir-{}-{}",
        std::process::id(),
        NEXT_TEMPORARY_ROOT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&temporary_root).expect("wrapper test directory must be created");
    let cargo_path = temporary_root.join("cargo");
    let output_path = temporary_root.join("cargo-output");
    let unusable_tmpdir = temporary_root.join("unusable-tmpdir");
    std::fs::write(
        &cargo_path,
        "#!/usr/bin/env bash\ncat \"$GAMEPULSE_TEST_CARGO_OUTPUT\"\nexit 0\n",
    )
    .expect("wrapper test cargo shim must be written");
    std::fs::set_permissions(&cargo_path, std::fs::Permissions::from_mode(0o700))
        .expect("wrapper test cargo shim must be executable");
    std::fs::write(&output_path, "").expect("wrapper test output must be written");
    std::fs::write(&unusable_tmpdir, "blocked").expect("unusable tmpdir marker must be written");

    let mut path_entries = vec![temporary_root.clone()];
    path_entries.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let output = Command::new("/bin/bash")
        .arg(repository_root.join("scripts/diagnostic_canary.sh"))
        .arg(mode)
        .current_dir(repository_root)
        .env(
            "PATH",
            std::env::join_paths(path_entries).expect("test PATH must be valid"),
        )
        .env("GAMEPULSE_TEST_CARGO_OUTPUT", &output_path)
        .env("TMPDIR", &unusable_tmpdir)
        .output()
        .expect("diagnostic wrapper must start");

    std::fs::remove_dir_all(&temporary_root).expect("wrapper test directory must be removed");
    output
}

fn run_mutation_harness_with_failing_cargo() -> std::process::Output {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_TEMPORARY_ROOT: AtomicUsize = AtomicUsize::new(0);

    let repository_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("worker-source manifest must remain two directories below the repository root");
    let temporary_root = std::env::temp_dir().join(format!(
        "gamepulse-diagnostic-mutation-{}-{}",
        std::process::id(),
        NEXT_TEMPORARY_ROOT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&temporary_root).expect("mutation test directory must be created");
    let cargo_path = temporary_root.join("cargo");
    std::fs::write(&cargo_path, "#!/usr/bin/env bash\nexit 42\n")
        .expect("mutation test cargo shim must be written");
    std::fs::set_permissions(&cargo_path, std::fs::Permissions::from_mode(0o700))
        .expect("mutation test cargo shim must be executable");

    let mut path_entries = vec![temporary_root.clone()];
    path_entries.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let output = Command::new("/bin/bash")
        .arg(repository_root.join("scripts/diagnostic_mutation.sh"))
        .current_dir(repository_root)
        .env(
            "PATH",
            std::env::join_paths(path_entries).expect("test PATH must be valid"),
        )
        .output()
        .expect("diagnostic mutation harness must start");

    std::fs::remove_dir_all(&temporary_root).expect("mutation test directory must be removed");
    output
}

fn print_report(report: &DiagnosticReport) {
    println!("{}", report.render());
}

fn live_opted_in() {
    if std::env::var(LIVE_OPT_IN).ok().as_deref() != Some("1") {
        panic!("set GAMEPULSE_M028_LIVE_DIAGNOSTIC=1 after separate owner authorization");
    }
}

#[tokio::test]
async fn diagnostic_fixture_report() {
    let report = DiagnosticCanary::fixture(
        DiagnosticMode::Fixture,
        [
            fixture_response(LISTING),
            fixture_response(M011_CRITIC),
            fixture_response(M011_USER),
        ],
    )
    .run()
    .await;

    assert_eq!(report.request_count, 3);
    assert_eq!(report.request_ceiling, 3);
    assert_eq!(report.terminal_verdict, TerminalVerdict::FixtureValidated);
    assert!(report.exchanges.iter().all(|exchange| {
        exchange.parser == ParserOutcome::Accepted
            && exchange.expected_content_type
            && exchange.utf8
            && exchange.json
            && exchange.numeric_total
    }));
    assert!(report.is_schema_valid());
    print_report(&report);
}

#[tokio::test]
async fn diagnostic_fixture_validates_positive_finder_and_review_continuation_modes() {
    let finder = DiagnosticCanary::fixture(DiagnosticMode::Finder, [fixture_response(LISTING)])
        .run()
        .await;
    assert_eq!(finder.request_count, 1);
    assert_eq!(finder.request_ceiling, 1);
    assert_eq!(finder.terminal_verdict, TerminalVerdict::ContractReady);
    assert!(finder.is_schema_valid());

    let review = DiagnosticCanary::fixture(
        DiagnosticMode::ReviewContinuation,
        [
            fixture_response(LISTING),
            fixture_response(M011_CRITIC),
            fixture_response(M011_USER),
        ],
    )
    .run()
    .await;
    assert_eq!(review.request_count, 3);
    assert_eq!(review.request_ceiling, 3);
    assert_eq!(review.terminal_verdict, TerminalVerdict::ContractReady);
    assert!(review.is_schema_valid());
}

#[tokio::test]
async fn diagnostic_fixture_status_and_body_failures_stop_before_a_follow_up() {
    for (status, expected_verdict) in [
        (403, TerminalVerdict::AccessDenied),
        (429, TerminalVerdict::RateLimited),
    ] {
        let report = DiagnosticCanary::fixture(
            DiagnosticMode::ReviewContinuation,
            [Ok(DiagnosticResponse::fixture(
                status,
                true,
                b"fixture-body".to_vec(),
            ))],
        )
        .run()
        .await;
        assert_eq!(report.request_count, 1);
        assert_eq!(report.terminal_verdict, expected_verdict);
        assert_eq!(report.exchanges[0].status_category, status_category(status));
        assert_eq!(report.exchanges[0].parser, ParserOutcome::Rejected);
        assert!(report.is_schema_valid());
    }

    for response in [
        DiagnosticResponse::fixture(200, false, b"fixture-body".to_vec()),
        DiagnosticResponse::fixture(200, true, vec![0xff]),
        DiagnosticResponse::fixture(200, true, b"not-json".to_vec()),
        DiagnosticResponse::fixture(200, true, vec![0; MAX_RESPONSE_BYTES + 1]),
    ] {
        let report = DiagnosticCanary::fixture(DiagnosticMode::Finder, [Ok(response)])
            .run()
            .await;
        assert_eq!(report.request_count, 1);
        assert_eq!(report.terminal_verdict, TerminalVerdict::SourceRejected);
        assert_eq!(report.exchanges[0].parser, ParserOutcome::Rejected);
        assert!(report.is_schema_valid());
    }
}

#[tokio::test]
async fn diagnostic_fixture_reports_no_candidate_as_valid_fail_closed_evidence() {
    let mut body: Value = serde_json::from_str(LISTING).expect("fixture must be JSON");
    body["data"]["items"] = serde_json::json!([]);
    let body = serde_json::to_string(&body).expect("fixture must serialize");
    let report = DiagnosticCanary::fixture(
        DiagnosticMode::ReviewContinuation,
        [fixture_response(&body)],
    )
    .run()
    .await;

    assert_eq!(report.request_count, 1);
    assert_eq!(report.terminal_verdict, TerminalVerdict::NoCandidate);
    assert!(report.is_schema_valid());
}

#[tokio::test]
async fn diagnostic_fixture_accepts_only_application_json_media_type() {
    for content_type in ["application/json", "Application/JSON; charset=utf-8"] {
        let report = DiagnosticCanary::fixture(
            DiagnosticMode::Finder,
            [Ok(DiagnosticResponse::fixture(
                200,
                is_application_json(content_type),
                LISTING.as_bytes().to_vec(),
            ))],
        )
        .run()
        .await;
        assert_eq!(report.exchanges[0].parser, ParserOutcome::Accepted);
        assert!(report.exchanges[0].expected_content_type);
    }

    for content_type in ["application/jsonp", "text/javascript", "text/json"] {
        let report = DiagnosticCanary::fixture(
            DiagnosticMode::Finder,
            [Ok(DiagnosticResponse::fixture(
                200,
                is_application_json(content_type),
                LISTING.as_bytes().to_vec(),
            ))],
        )
        .run()
        .await;
        assert_eq!(report.exchanges[0].parser, ParserOutcome::Rejected);
        assert!(!report.exchanges[0].expected_content_type);
    }
}

#[tokio::test]
async fn diagnostic_fixture_reports_continuation_and_href_shapes_without_source_values() {
    let cases = [
        (
            json_without_next(LISTING),
            ContinuationPresence::Missing,
            HrefPresence::NotApplicable,
            ParserOutcome::Accepted,
        ),
        (
            json_with_next(LISTING, Value::Null),
            ContinuationPresence::Null,
            HrefPresence::NotApplicable,
            ParserOutcome::Rejected,
        ),
        (
            json_with_next(LISTING, serde_json::json!({})),
            ContinuationPresence::Object,
            HrefPresence::Missing,
            ParserOutcome::Rejected,
        ),
        (
            json_with_next(LISTING, serde_json::json!({"href": null})),
            ContinuationPresence::Object,
            HrefPresence::Null,
            ParserOutcome::Rejected,
        ),
        (
            LISTING.to_owned(),
            ContinuationPresence::Object,
            HrefPresence::String,
            ParserOutcome::Accepted,
        ),
    ];

    for (body, continuation, href, parser) in cases {
        let report = DiagnosticCanary::fixture(DiagnosticMode::Finder, [fixture_response(&body)])
            .run()
            .await;
        let exchange = &report.exchanges[0];
        assert_eq!(exchange.continuation_presence, continuation);
        assert_eq!(exchange.href_presence, href);
        assert_eq!(exchange.parser, parser);
        assert!(report.is_schema_valid());
    }
}

#[tokio::test]
async fn diagnostic_fixture_reports_valid_and_invalid_link_relations() {
    let valid = DiagnosticCanary::fixture(DiagnosticMode::Finder, [fixture_response(LISTING)])
        .run()
        .await;
    assert_eq!(valid.exchanges[0].parser, ParserOutcome::Accepted);
    assert!(valid.is_schema_valid());
    assert_eq!(
        valid.exchanges[0].link_checks,
        LinkChecks {
            scheme: true,
            host: true,
            path: true,
            query: true,
            progression: true,
            limit: true,
            total_boundary: true,
        }
    );

    for body in [
        LISTING.replacen("https://", "http://", 1),
        LISTING.replace("backend.metacritic.com", "invalid.example.test"),
        LISTING.replace("/finder/metacritic/web", "/finder/other/web"),
        LISTING.replace("offset=20&limit=20", "offset=0&limit=20"),
        LISTING.replace("offset=20&limit=20", "offset=20&limit=10"),
        LISTING.replace("\"totalResults\": 42", "\"totalResults\": 20"),
        LISTING.replace("offset=20&limit=20", "offset=20&offset=20&limit=20"),
    ] {
        let report = DiagnosticCanary::fixture(DiagnosticMode::Finder, [fixture_response(&body)])
            .run()
            .await;
        let exchange = &report.exchanges[0];
        assert_eq!(exchange.parser, ParserOutcome::Rejected);
        assert_eq!(exchange.safe_category, SafeCategory::OtherMandatoryStage);
        assert!(report.is_schema_valid());
        assert!(
            !exchange.link_checks.scheme
                || !exchange.link_checks.host
                || !exchange.link_checks.path
                || !exchange.link_checks.query
                || !exchange.link_checks.progression
                || !exchange.link_checks.limit
                || !exchange.link_checks.total_boundary
        );
    }
}

#[tokio::test]
async fn diagnostic_fixture_preserves_critic_clamp_and_user_first_page_strictness() {
    let critic = DiagnosticCanary::fixture(
        DiagnosticMode::Finder,
        [fixture_response(M015_CRITIC_CLAMP)],
    )
    .execute(ProbeRequest::Review {
        kind: ReviewKind::Critic,
        candidate_slug: "example-game",
    })
    .await;
    assert_eq!(critic.report.parser, ParserOutcome::Accepted);
    assert!(critic.report.link_checks.progression);
    assert!(critic.report.link_checks.limit);

    let user_body = M015_CRITIC_CLAMP.replace("/critic/", "/user/");
    let user = DiagnosticCanary::fixture(DiagnosticMode::Finder, [fixture_response(&user_body)])
        .execute(ProbeRequest::Review {
            kind: ReviewKind::User,
            candidate_slug: "example-game",
        })
        .await;
    assert_eq!(user.report.parser, ParserOutcome::Rejected);
    assert_eq!(
        user.report.safe_category,
        SafeCategory::ReviewContinuationLink
    );
    assert!(!user.report.link_checks.limit);
}

#[tokio::test]
async fn diagnostic_fixture_accepts_only_the_review_terminal_placeholder_shape() {
    let terminal = DiagnosticCanary::fixture(
        DiagnosticMode::Finder,
        [fixture_response(M017_REVIEW_TERMINAL_EMPTY)],
    )
    .execute(ProbeRequest::Review {
        kind: ReviewKind::User,
        candidate_slug: "example-game",
    })
    .await;
    assert_eq!(
        terminal.report.continuation_presence,
        ContinuationPresence::Object
    );
    assert_eq!(terminal.report.href_presence, HrefPresence::Missing);
    assert_eq!(terminal.report.parser, ParserOutcome::Accepted);

    let non_terminal =
        M017_REVIEW_TERMINAL_EMPTY.replace("\"totalResults\": 0", "\"totalResults\": 1");
    let rejected =
        DiagnosticCanary::fixture(DiagnosticMode::Finder, [fixture_response(&non_terminal)])
            .execute(ProbeRequest::Review {
                kind: ReviewKind::User,
                candidate_slug: "example-game",
            })
            .await;
    assert_eq!(rejected.report.parser, ParserOutcome::Rejected);
    assert_eq!(
        rejected.report.safe_category,
        SafeCategory::ReviewContinuationLink
    );
}

#[tokio::test]
async fn diagnostic_fixture_review_link_rejection_remains_parseable() {
    let non_terminal =
        M017_REVIEW_TERMINAL_EMPTY.replace("\"totalResults\": 0", "\"totalResults\": 1");
    let report = DiagnosticCanary::fixture(
        DiagnosticMode::ReviewContinuation,
        [
            fixture_response(LISTING),
            fixture_response(M011_CRITIC),
            fixture_response(&non_terminal),
        ],
    )
    .run()
    .await;

    assert_eq!(report.request_count, 3);
    assert_eq!(report.terminal_verdict, TerminalVerdict::SourceRejected);
    assert_eq!(
        report.exchanges[2].safe_category,
        SafeCategory::ReviewContinuationLink
    );
    assert!(report.is_schema_valid());
}

#[tokio::test]
async fn diagnostic_fixture_stops_early_and_fails_closed_at_the_request_ceiling() {
    let canary = DiagnosticCanary::fixture(
        DiagnosticMode::ReviewContinuation,
        [
            Ok(DiagnosticResponse::fixture(403, true, b"ignored".to_vec())),
            fixture_response(M011_CRITIC),
        ],
    );
    let report = canary.run().await;
    assert_eq!(report.request_count, 1);
    assert_eq!(report.terminal_verdict, TerminalVerdict::AccessDenied);

    let mut canary = DiagnosticCanary::fixture(
        DiagnosticMode::ReviewContinuation,
        [
            fixture_response(LISTING),
            fixture_response(M011_CRITIC),
            fixture_response(M011_USER),
            fixture_response(LISTING),
        ],
    );
    let first = canary.execute(ProbeRequest::Finder).await;
    let second = canary
        .execute(ProbeRequest::Review {
            kind: ReviewKind::Critic,
            candidate_slug: "example-game",
        })
        .await;
    let third = canary
        .execute(ProbeRequest::Review {
            kind: ReviewKind::User,
            candidate_slug: "example-game",
        })
        .await;
    let fourth = canary.execute(ProbeRequest::Finder).await;
    assert_eq!(first.report.parser, ParserOutcome::Accepted);
    assert_eq!(second.report.parser, ParserOutcome::Accepted);
    assert_eq!(third.report.parser, ParserOutcome::Accepted);
    assert!(fourth.budget_exhausted);
    assert_eq!(fourth.report.status_category, StatusCategory::NotAttempted);
    assert_eq!(canary.budget.attempts, 3);
    assert_eq!(canary.transport.fixture_calls(), 3);
    let exhausted = canary.finish(
        vec![first.report, second.report, third.report],
        true,
        fourth.budget_exhausted,
    );
    assert_eq!(exhausted.request_count, 3);
    assert_eq!(
        exhausted.terminal_verdict,
        TerminalVerdict::RequestBudgetExhausted
    );
    assert!(exhausted.is_schema_valid());
}

#[tokio::test]
async fn diagnostic_privacy_output_never_contains_fixture_source_data() {
    let private_fixture_marker = "M028_PRIVATE_FIXTURE_MARKER_DO_NOT_EMIT";
    let report = DiagnosticCanary::fixture(
        DiagnosticMode::Finder,
        [Ok(DiagnosticResponse::fixture(
            200,
            true,
            private_fixture_marker.as_bytes().to_vec(),
        ))],
    )
    .run()
    .await;
    let rendered = report.render();

    for forbidden in [
        private_fixture_marker,
        "example-game",
        "Example Game",
        "backend.metacritic.com",
        "http://",
        "https://",
        "body",
        "slug",
        "title",
    ] {
        assert!(
            !rendered.contains(forbidden),
            "aggregate report must not contain source-derived fixture data"
        );
    }
    assert_eq!(report.terminal_verdict, TerminalVerdict::SourceRejected);
}

#[test]
fn diagnostic_report_schema_rejects_inconsistent_terminal_evidence() {
    let mut report = report_with_terminal(DiagnosticMode::Finder, TerminalVerdict::ContractReady);
    assert!(report.is_schema_valid());

    report.terminal_verdict = TerminalVerdict::SourceRejected;
    assert!(!report.is_schema_valid());

    let mut invalid_link =
        report_with_terminal(DiagnosticMode::Finder, TerminalVerdict::ContractReady);
    invalid_link.exchanges[0].href_presence = HrefPresence::String;
    assert!(!invalid_link.is_schema_valid());
}

#[test]
fn diagnostic_report_schema_rejects_impossible_exchange_truth_table_combinations() {
    let valid = report_with_terminal(DiagnosticMode::Finder, TerminalVerdict::ContractReady);
    assert!(valid.is_schema_valid());

    let mut accepted_not_checked = valid.clone();
    accepted_not_checked.exchanges[0].continuation_presence = ContinuationPresence::NotChecked;
    assert!(!accepted_not_checked.is_schema_valid());

    let mut accepted_finder_placeholder = valid.clone();
    accepted_finder_placeholder.exchanges[0].continuation_presence = ContinuationPresence::Object;
    accepted_finder_placeholder.exchanges[0].href_presence = HrefPresence::Missing;
    assert!(!accepted_finder_placeholder.is_schema_valid());

    let mut accepted_unchecked_link = valid.clone();
    accepted_unchecked_link.exchanges[0].continuation_presence = ContinuationPresence::Object;
    accepted_unchecked_link.exchanges[0].href_presence = HrefPresence::String;
    assert!(!accepted_unchecked_link.is_schema_valid());

    let mut rejected_not_attempted = valid.clone();
    rejected_not_attempted.exchanges[0].parser = ParserOutcome::Rejected;
    rejected_not_attempted.exchanges[0].status_category = StatusCategory::NotAttempted;
    rejected_not_attempted.exchanges[0].continuation_presence = ContinuationPresence::NotChecked;
    rejected_not_attempted.exchanges[0].href_presence = HrefPresence::NotApplicable;
    rejected_not_attempted.terminal_verdict = TerminalVerdict::SourceRejected;
    assert!(!rejected_not_attempted.is_schema_valid());

    let mut rejected_inconsistent_presence = valid.clone();
    rejected_inconsistent_presence.exchanges[0].parser = ParserOutcome::Rejected;
    rejected_inconsistent_presence.exchanges[0].continuation_presence =
        ContinuationPresence::Missing;
    rejected_inconsistent_presence.exchanges[0].href_presence = HrefPresence::String;
    rejected_inconsistent_presence.terminal_verdict = TerminalVerdict::SourceRejected;
    assert!(!rejected_inconsistent_presence.is_schema_valid());

    let mut finder_review_category = valid.clone();
    finder_review_category.exchanges[0].parser = ParserOutcome::Rejected;
    finder_review_category.exchanges[0].continuation_presence = ContinuationPresence::Object;
    finder_review_category.exchanges[0].href_presence = HrefPresence::String;
    finder_review_category.exchanges[0].link_checks = LinkChecks {
        scheme: true,
        host: true,
        path: true,
        query: true,
        progression: true,
        limit: true,
        total_boundary: true,
    };
    finder_review_category.exchanges[0].safe_category = SafeCategory::ReviewContinuationLink;
    finder_review_category.terminal_verdict = TerminalVerdict::SourceRejected;
    assert!(!finder_review_category.is_schema_valid());

    let mut incomplete_review_category = report_with_terminal(
        DiagnosticMode::ReviewContinuation,
        TerminalVerdict::ContractReady,
    );
    incomplete_review_category.exchanges[2].parser = ParserOutcome::Rejected;
    incomplete_review_category.exchanges[2].continuation_presence = ContinuationPresence::Object;
    incomplete_review_category.exchanges[2].href_presence = HrefPresence::String;
    incomplete_review_category.exchanges[2].safe_category = SafeCategory::ReviewContinuationLink;
    incomplete_review_category.exchanges[2].numeric_total = false;
    incomplete_review_category.terminal_verdict = TerminalVerdict::SourceRejected;
    assert!(!incomplete_review_category.is_schema_valid());
}

#[test]
fn diagnostic_wrapper_accepts_positive_finder_and_review_reports() {
    for (mode, test_name, report) in [
        (
            "fixture",
            "diagnostic_fixture_report",
            report_with_terminal(DiagnosticMode::Fixture, TerminalVerdict::FixtureValidated),
        ),
        (
            "finder",
            "diagnostic_live_finder",
            report_with_terminal(DiagnosticMode::Finder, TerminalVerdict::ContractReady),
        ),
        (
            "review-continuation",
            "diagnostic_live_review_continuation",
            report_with_terminal(
                DiagnosticMode::ReviewContinuation,
                TerminalVerdict::ContractReady,
            ),
        ),
    ] {
        assert!(report.is_schema_valid());
        let rendered = report.render();
        let output =
            run_wrapper_with_cargo_output(mode, &cargo_harness_output(test_name, &rendered));
        assert!(output.status.success());
        assert_eq!(output.stdout, format!("{rendered}\n").as_bytes());
        assert_eq!(output.stderr, b"");
    }
}

#[test]
fn diagnostic_wrapper_preserves_every_fail_closed_verdict_with_exit_three() {
    for terminal_verdict in [
        TerminalVerdict::AccessDenied,
        TerminalVerdict::RateLimited,
        TerminalVerdict::SourceRejected,
        TerminalVerdict::NoCandidate,
        TerminalVerdict::RequestBudgetExhausted,
    ] {
        let report = report_with_terminal(DiagnosticMode::ReviewContinuation, terminal_verdict);
        assert!(report.is_schema_valid());
        let rendered = report.render();
        let output = run_wrapper_with_cargo_output(
            "review-continuation",
            &cargo_harness_output("diagnostic_live_review_continuation", &rendered),
        );
        assert_eq!(output.status.code(), Some(3));
        assert_eq!(output.stdout, format!("{rendered}\n").as_bytes());
        assert_eq!(output.stderr, b"");
    }
}

#[test]
fn diagnostic_wrapper_fails_closed_on_invalid_or_noisy_zero_exit_output() {
    let valid =
        report_with_terminal(DiagnosticMode::Finder, TerminalVerdict::ContractReady).render();
    let harness = |report: &str| cargo_harness_output("diagnostic_live_finder", report);
    let extra_sensitive_field = format!(
        "{},\"source_path\":\"marker\"}}",
        valid
            .strip_suffix('}')
            .expect("report must end in an object")
    );
    let mut invalid_cases = vec![
        harness(""),
        harness("{malformed"),
        harness(&format!("{valid}\n{valid}")),
        harness(&valid.replacen(
            "\"mode\":\"finder\",",
            "\"mode\":\"finder\",\"mode\":\"finder\",",
            1,
        )),
        harness(&valid.replace("\"request_count\":1", "\"request_count\":\"1\"")),
        harness(&valid.replace("\"request_count\":1", "\"request_count\":2")),
        harness(&valid.replace("\"request_ceiling\":1", "\"request_ceiling\":3")),
        harness(&valid.replace("\"request\":\"finder\"", "\"request\":\"critic_review\"")),
        harness(&valid.replace(
            "\"expected_content_type\":true",
            "\"expected_content_type\":\"true\"",
        )),
        harness(&valid.replace(
            "\"terminal_verdict\":\"contract_ready\"",
            "\"terminal_verdict\":\"source_rejected\"",
        )),
        harness(&extra_sensitive_field),
        format!("source noise\n{}", harness(&valid)),
    ];

    let mut accepted_not_checked =
        report_with_terminal(DiagnosticMode::Finder, TerminalVerdict::ContractReady);
    accepted_not_checked.exchanges[0].continuation_presence = ContinuationPresence::NotChecked;
    invalid_cases.push(harness(
        &serde_json::to_string(&accepted_not_checked)
            .expect("invalid aggregate report must serialize for wrapper validation"),
    ));

    let mut accepted_finder_placeholder =
        report_with_terminal(DiagnosticMode::Finder, TerminalVerdict::ContractReady);
    accepted_finder_placeholder.exchanges[0].continuation_presence = ContinuationPresence::Object;
    accepted_finder_placeholder.exchanges[0].href_presence = HrefPresence::Missing;
    invalid_cases.push(harness(
        &serde_json::to_string(&accepted_finder_placeholder)
            .expect("invalid aggregate report must serialize for wrapper validation"),
    ));

    let mut accepted_unchecked_link =
        report_with_terminal(DiagnosticMode::Finder, TerminalVerdict::ContractReady);
    accepted_unchecked_link.exchanges[0].continuation_presence = ContinuationPresence::Object;
    accepted_unchecked_link.exchanges[0].href_presence = HrefPresence::String;
    invalid_cases.push(harness(
        &serde_json::to_string(&accepted_unchecked_link)
            .expect("invalid aggregate report must serialize for wrapper validation"),
    ));

    let mut rejected_not_attempted =
        report_with_terminal(DiagnosticMode::Finder, TerminalVerdict::ContractReady);
    rejected_not_attempted.exchanges[0].parser = ParserOutcome::Rejected;
    rejected_not_attempted.exchanges[0].status_category = StatusCategory::NotAttempted;
    rejected_not_attempted.exchanges[0].continuation_presence = ContinuationPresence::NotChecked;
    rejected_not_attempted.exchanges[0].href_presence = HrefPresence::NotApplicable;
    rejected_not_attempted.terminal_verdict = TerminalVerdict::SourceRejected;
    invalid_cases.push(harness(
        &serde_json::to_string(&rejected_not_attempted)
            .expect("invalid aggregate report must serialize for wrapper validation"),
    ));

    let mut rejected_inconsistent_presence =
        report_with_terminal(DiagnosticMode::Finder, TerminalVerdict::ContractReady);
    rejected_inconsistent_presence.exchanges[0].parser = ParserOutcome::Rejected;
    rejected_inconsistent_presence.exchanges[0].href_presence = HrefPresence::String;
    rejected_inconsistent_presence.terminal_verdict = TerminalVerdict::SourceRejected;
    invalid_cases.push(harness(
        &serde_json::to_string(&rejected_inconsistent_presence)
            .expect("invalid aggregate report must serialize for wrapper validation"),
    ));

    let mut finder_review_category =
        report_with_terminal(DiagnosticMode::Finder, TerminalVerdict::ContractReady);
    finder_review_category.exchanges[0].parser = ParserOutcome::Rejected;
    finder_review_category.exchanges[0].continuation_presence = ContinuationPresence::Object;
    finder_review_category.exchanges[0].href_presence = HrefPresence::String;
    finder_review_category.exchanges[0].link_checks = LinkChecks {
        scheme: true,
        host: true,
        path: true,
        query: true,
        progression: true,
        limit: true,
        total_boundary: true,
    };
    finder_review_category.exchanges[0].safe_category = SafeCategory::ReviewContinuationLink;
    finder_review_category.terminal_verdict = TerminalVerdict::SourceRejected;
    invalid_cases.push(harness(
        &serde_json::to_string(&finder_review_category)
            .expect("invalid aggregate report must serialize for wrapper validation"),
    ));

    for cargo_stdout in invalid_cases {
        let output = run_wrapper_with_cargo_output("finder", &cargo_stdout);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(output.stdout, b"");
        assert_eq!(output.stderr, b"diagnostic command failed\n");
    }

    let mut incomplete_review_category = report_with_terminal(
        DiagnosticMode::ReviewContinuation,
        TerminalVerdict::ContractReady,
    );
    incomplete_review_category.exchanges[2].parser = ParserOutcome::Rejected;
    incomplete_review_category.exchanges[2].continuation_presence = ContinuationPresence::Object;
    incomplete_review_category.exchanges[2].href_presence = HrefPresence::String;
    incomplete_review_category.exchanges[2].safe_category = SafeCategory::ReviewContinuationLink;
    incomplete_review_category.exchanges[2].numeric_total = false;
    incomplete_review_category.terminal_verdict = TerminalVerdict::SourceRejected;
    let output = run_wrapper_with_cargo_output(
        "review-continuation",
        &cargo_harness_output(
            "diagnostic_live_review_continuation",
            &serde_json::to_string(&incomplete_review_category)
                .expect("invalid aggregate report must serialize for wrapper validation"),
        ),
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"");
    assert_eq!(output.stderr, b"diagnostic command failed\n");
}

#[test]
fn diagnostic_wrapper_rejects_every_noncanonical_controlled_transcript() {
    let valid =
        report_with_terminal(DiagnosticMode::Finder, TerminalVerdict::ContractReady).render();
    let summary = "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s";
    let canonical = cargo_harness_output("diagnostic_live_finder", &valid);
    let invalid_cases = [
        format!("source noise\n{canonical}"),
        canonical.replacen("\n.\n", "\n.\n.\n", 1),
        format!("\n{valid}\nrunning 1 test\n.\n{summary}\n"),
        format!("\nrunning 1 test\n.\n{summary}\n"),
        format!("\nrunning 1 test\n{valid}\n{summary}\n"),
        format!("\nrunning 1 test\n{valid}\n.\n"),
        format!("{canonical}\n"),
    ];

    for cargo_stdout in invalid_cases {
        let output = run_wrapper_with_cargo_output("finder", &cargo_stdout);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(output.stdout, b"");
        assert_eq!(output.stderr, b"diagnostic command failed\n");
    }
}

#[test]
fn diagnostic_wrapper_hides_unusable_tmpdir_details() {
    let output = run_wrapper_with_unusable_tmpdir("finder");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"");
    assert_eq!(output.stderr, b"diagnostic command failed\n");
}

#[test]
fn diagnostic_mutation_harness_rejects_infrastructure_failures() {
    let output = run_mutation_harness_with_failing_cargo();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(output.stdout, b"request-ceiling: baseline_failed\n");
    assert_eq!(output.stderr, b"");
}

#[test]
fn diagnostic_quiet_cli_redacts_opt_in_marker_and_local_paths() {
    let repository_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("worker-source manifest must remain two directories below the repository root");
    let marker = "M028_PROCESS_OPT_IN_MARKER_DO_NOT_EMIT";
    let output = Command::new("bash")
        .arg(repository_root.join("scripts/diagnostic_canary.sh"))
        .arg("finder")
        .current_dir(repository_root)
        .env(LIVE_OPT_IN, marker)
        .output()
        .expect("quiet diagnostic wrapper must start");
    let stdout = String::from_utf8(output.stdout).expect("wrapper stdout must be UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("wrapper stderr must be UTF-8");

    assert!(!output.status.success());
    assert_eq!(stdout, "");
    assert_eq!(stderr, "diagnostic command failed\n");
    for forbidden in [
        marker,
        repository_root.to_str().expect("UTF-8 root"),
        "target/debug",
    ] {
        assert!(!stdout.contains(forbidden));
        assert!(!stderr.contains(forbidden));
    }
}

#[tokio::test]
#[ignore = "requires separate owner authorization and one anonymous public finder request"]
async fn diagnostic_live_finder() {
    live_opted_in();
    let report = DiagnosticCanary::live(DiagnosticMode::Finder)
        .expect("safe live transport configuration must be available")
        .run()
        .await;
    assert!(report.request_count <= 1);
    print_report(&report);
}

#[tokio::test]
#[ignore = "requires separate owner authorization and at most three anonymous public requests"]
async fn diagnostic_live_review_continuation() {
    live_opted_in();
    let report = DiagnosticCanary::live(DiagnosticMode::ReviewContinuation)
        .expect("safe live transport configuration must be available")
        .run()
        .await;
    assert!(report.request_count <= 3);
    print_report(&report);
}
