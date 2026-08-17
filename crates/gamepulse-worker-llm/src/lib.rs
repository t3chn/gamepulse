#![forbid(unsafe_code)]

//! Local deterministic review-summary worker lane.

use std::sync::{Arc, Mutex};

use gamepulse_application::{
    JobHandler, JobHandlerFailure, JobHandlerFuture, JobHandlerResult, ReviewInput, ReviewPolarity,
    ReviewSummarizer, ReviewSummary, ReviewSummaryOutput, ReviewSummaryRequest, ReviewSummaryStore,
    RuntimeJobType, TypedJob,
};

const REVIEW_SUMMARY_FAILURE: &str = "local review summary failed";

/// A deterministic, local extractive fallback. It has no model, provider, credential,
/// configuration, SDK, or network dependency. It retains only already bounded excerpts.
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalExtractiveReviewSummarizer;

impl ReviewSummarizer for LocalExtractiveReviewSummarizer {
    type Error = std::convert::Infallible;

    fn summarize(&self, input: &ReviewInput) -> Result<ReviewSummaryOutput, Self::Error> {
        if input.excerpts().is_empty() {
            return Ok(ReviewSummaryOutput::Unavailable);
        }
        let mut likes = Vec::new();
        let mut dislikes = Vec::new();
        for excerpt in input.excerpts() {
            let text = excerpt.as_str();
            match classify_review_excerpt(text, excerpt.polarity()) {
                ExcerptSentiment::Like => push_summary_item(&mut likes, text),
                ExcerptSentiment::Dislike => push_summary_item(&mut dislikes, text),
                ExcerptSentiment::Mixed => {
                    push_summary_item(&mut likes, text);
                    push_summary_item(&mut dislikes, text);
                }
                ExcerptSentiment::Unknown => {}
            }
        }
        Ok(ReviewSummaryOutput::available(likes, dislikes)
            .expect("bounded local excerpts produce bounded summary items"))
    }
}

fn push_summary_item(target: &mut Vec<String>, text: &str) {
    if target.len() < 3 && !target.iter().any(|existing| existing == text) {
        target.push(text.to_owned());
    }
}

/// The explainable result of classifying one persisted review excerpt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExcerptSentiment {
    Like,
    Dislike,
    Mixed,
    Unknown,
}

/// Classify explicit English sentiment with a bounded negation window. Explicit text wins over a
/// retained score-derived polarity; polarity is used only when the text has no known sentiment.
pub fn classify_review_excerpt(text: &str, polarity: Option<ReviewPolarity>) -> ExcerptSentiment {
    let mut positive = false;
    let mut negative = false;
    let mut negation_window = 0_u8;

    for token in text
        .split(|character: char| !character.is_ascii_alphabetic())
        .filter(|token| !token.is_empty())
    {
        let token = token.to_ascii_lowercase();
        if is_negator(&token) {
            negation_window = 3;
            continue;
        }

        let sentiment = match token.as_str() {
            "good" | "great" | "excellent" | "enjoyable" | "fun" | "love" | "liked" | "strong"
            | "polished" | "praise" | "satisfying" => Some(true),
            "bad" | "boring" | "poor" | "weak" | "dislike" | "awful" | "terrible"
            | "frustrating" | "broken" | "hate" => Some(false),
            _ => None,
        };

        if let Some(is_positive) = sentiment {
            match (is_positive, negation_window > 0) {
                (true, false) | (false, true) => positive = true,
                (false, false) | (true, true) => negative = true,
            }
            negation_window = 0;
        } else {
            negation_window = negation_window.saturating_sub(1);
        }
    }

    match (positive, negative) {
        (true, false) => ExcerptSentiment::Like,
        (false, true) => ExcerptSentiment::Dislike,
        (true, true) => ExcerptSentiment::Mixed,
        (false, false) => match polarity {
            Some(ReviewPolarity::Positive) => ExcerptSentiment::Like,
            Some(ReviewPolarity::Negative) => ExcerptSentiment::Dislike,
            None => ExcerptSentiment::Unknown,
        },
    }
}

fn is_negator(token: &str) -> bool {
    matches!(token, "not" | "never" | "no" | "hardly" | "without")
}

/// The LLM-lane adapter for a durable, fingerprint-fenced summary job.
pub struct ReviewSummaryHandler<S, P> {
    store: Arc<Mutex<S>>,
    summarizer: Arc<P>,
}

impl<S, P> ReviewSummaryHandler<S, P> {
    pub fn new(store: Arc<Mutex<S>>, summarizer: P) -> Self {
        Self {
            store,
            summarizer: Arc::new(summarizer),
        }
    }
}

impl<S, P> JobHandler for ReviewSummaryHandler<S, P>
where
    S: ReviewSummaryStore + Send + 'static,
    S::Error: Send + 'static,
    P: ReviewSummarizer + Send + Sync + 'static,
    P::Error: Send + 'static,
{
    fn job_type(&self) -> RuntimeJobType {
        RuntimeJobType::LlmReviewSummary
    }

    fn handle(&self, job: TypedJob) -> JobHandlerFuture {
        let store = Arc::clone(&self.store);
        let summarizer = Arc::clone(&self.summarizer);
        Box::pin(async move {
            let Ok(request) = ReviewSummaryRequest::from_work_reference(job.work_ref()) else {
                return JobHandlerResult::Failed(JobHandlerFailure::new(REVIEW_SUMMARY_FAILURE));
            };
            let input = match store.lock() {
                Ok(mut store) => match store.load_review_input(&request) {
                    Ok(input) => input,
                    Err(_) => {
                        return JobHandlerResult::Failed(JobHandlerFailure::new(
                            REVIEW_SUMMARY_FAILURE,
                        ));
                    }
                },
                Err(_) => {
                    return JobHandlerResult::Failed(JobHandlerFailure::new(
                        REVIEW_SUMMARY_FAILURE,
                    ));
                }
            };
            let Some(input) = input else {
                // The durable refresh moved on. Fenced persistence would reject this job; treat
                // the obsolete attempt as settled without manufacturing or overwriting output.
                return JobHandlerResult::Succeeded;
            };
            let output = match summarizer.summarize(&input) {
                Ok(output) => output,
                Err(_) => {
                    return JobHandlerResult::Failed(JobHandlerFailure::new(
                        REVIEW_SUMMARY_FAILURE,
                    ));
                }
            };
            let summary = ReviewSummary::new(request, output);
            match store.lock() {
                Ok(mut store) => match store.persist_review_summary(&summary) {
                    Ok(_) => JobHandlerResult::Succeeded,
                    Err(_) => {
                        JobHandlerResult::Failed(JobHandlerFailure::new(REVIEW_SUMMARY_FAILURE))
                    }
                },
                Err(_) => JobHandlerResult::Failed(JobHandlerFailure::new(REVIEW_SUMMARY_FAILURE)),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use gamepulse_application::{ReviewExcerpt, ReviewInput, ReviewKind, SourceProductId};

    use super::*;

    const SENTIMENT_CASES: &str = include_str!("../tests/fixtures/review-sentiment-cases.txt");

    fn input(kind: ReviewKind, excerpts: &[&str]) -> ReviewInput {
        ReviewInput::new(
            SourceProductId::new(101).expect("test identity must be valid"),
            kind,
            excerpts
                .iter()
                .map(|excerpt| ReviewExcerpt::new(*excerpt).expect("test excerpt must be valid"))
                .collect(),
        )
        .expect("test input must be valid")
    }

    #[test]
    fn local_fallback_is_deterministic_and_does_not_invent_empty_input_content() {
        let fallback = LocalExtractiveReviewSummarizer;
        let critic = input(
            ReviewKind::Critic,
            &[
                "Critics praise the synthetic controls.",
                "Critics dislike the boring finale.",
            ],
        );
        let user = input(ReviewKind::User, &[]);

        assert_eq!(
            fallback.summarize(&critic).expect("fallback is infallible"),
            ReviewSummaryOutput::available(
                vec!["Critics praise the synthetic controls.".to_owned()],
                vec!["Critics dislike the boring finale.".to_owned()],
            )
            .expect("test summary must be valid")
        );
        assert_eq!(
            fallback.summarize(&user).expect("fallback is infallible"),
            ReviewSummaryOutput::Unavailable
        );
    }

    #[test]
    fn local_fallback_recognizes_negative_tokens_at_excerpt_and_sentence_boundaries() {
        let fallback = LocalExtractiveReviewSummarizer;
        let critic = input(
            ReviewKind::Critic,
            &[
                "Poor synthetic performance.",
                "Synthetic controls. Weak synthetic ending.",
            ],
        );

        assert_eq!(
            fallback.summarize(&critic).expect("fallback is infallible"),
            ReviewSummaryOutput::available(
                Vec::new(),
                vec![
                    "Poor synthetic performance.".to_owned(),
                    "Synthetic controls. Weak synthetic ending.".to_owned(),
                ],
            )
            .expect("test summary must be valid")
        );
    }

    #[test]
    fn critic_and_user_fixture_cases_classify_explicit_negation_mixed_and_unknown_text() {
        let cases = SENTIMENT_CASES
            .lines()
            .map(|line| {
                let mut fields = line.splitn(3, '|');
                let name = fields.next().expect("fixture name must be present");
                let expected = fields.next().expect("fixture result must be present");
                let text = fields.next().expect("fixture excerpt must be present");
                (name, expected, text)
            })
            .collect::<Vec<_>>();
        let expected = [
            ("positive", ExcerptSentiment::Like),
            ("negative", ExcerptSentiment::Dislike),
            ("negated-positive", ExcerptSentiment::Dislike),
            ("negated-negative", ExcerptSentiment::Like),
            ("mixed", ExcerptSentiment::Mixed),
            ("unknown", ExcerptSentiment::Unknown),
        ];

        for kind in ReviewKind::ALL {
            let input = input(
                kind,
                &cases.iter().map(|(_, _, text)| *text).collect::<Vec<_>>(),
            );
            for ((name, expected_label, text), (expected_name, expected_sentiment)) in
                cases.iter().zip(expected)
            {
                assert_eq!(name, &expected_name);
                assert_eq!(
                    *expected_label,
                    match expected_sentiment {
                        ExcerptSentiment::Like => "like",
                        ExcerptSentiment::Dislike => "dislike",
                        ExcerptSentiment::Mixed => "mixed",
                        ExcerptSentiment::Unknown => "unknown",
                    }
                );
                assert_eq!(classify_review_excerpt(text, None), expected_sentiment);
            }
            assert_eq!(
                LocalExtractiveReviewSummarizer
                    .summarize(&input)
                    .expect("fixture fallback must be infallible"),
                ReviewSummaryOutput::available(
                    vec![
                        cases[0].2.to_owned(),
                        cases[3].2.to_owned(),
                        cases[4].2.to_owned(),
                    ],
                    vec![
                        cases[1].2.to_owned(),
                        cases[2].2.to_owned(),
                        cases[4].2.to_owned(),
                    ],
                )
                .expect("fixture output must be valid")
            );
        }
    }

    #[test]
    fn retained_score_polarity_is_used_only_when_text_is_unknown() {
        let excerpt = ReviewExcerpt::with_polarity(
            "The release date is Tuesday.",
            Some(ReviewPolarity::Positive),
        )
        .expect("test excerpt must be valid");

        assert_eq!(
            classify_review_excerpt(excerpt.as_str(), excerpt.polarity()),
            ExcerptSentiment::Like
        );
        assert_eq!(
            classify_review_excerpt("The controls are not good.", Some(ReviewPolarity::Positive)),
            ExcerptSentiment::Dislike
        );
    }

    #[derive(Clone, Copy)]
    struct TestError;

    struct FailingSummarizer;

    impl ReviewSummarizer for FailingSummarizer {
        type Error = TestError;

        fn summarize(&self, _input: &ReviewInput) -> Result<ReviewSummaryOutput, TestError> {
            Err(TestError)
        }
    }

    #[test]
    fn summarizer_failure_is_an_explicit_error_instead_of_an_invented_summary() {
        assert!(
            FailingSummarizer
                .summarize(&input(ReviewKind::Critic, &["Synthetic input."]))
                .is_err()
        );
    }
}
