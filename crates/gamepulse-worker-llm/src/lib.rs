#![forbid(unsafe_code)]

//! Local deterministic review-summary worker lane.

use std::sync::{Arc, Mutex};

use gamepulse_application::{
    JobHandler, JobHandlerFailure, JobHandlerFuture, JobHandlerResult, ReviewInput,
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
            let target = if contains_negative_token(text) {
                &mut dislikes
            } else {
                &mut likes
            };
            if target.len() < 3 && !target.iter().any(|existing: &String| existing == text) {
                target.push(text.to_owned());
            }
        }
        Ok(ReviewSummaryOutput::available(likes, dislikes)
            .expect("bounded local excerpts produce bounded summary items"))
    }
}

fn contains_negative_token(text: &str) -> bool {
    text.to_ascii_lowercase()
        .split(|character: char| !character.is_ascii_alphabetic())
        .any(|token| {
            matches!(
                token,
                "bad" | "boring" | "poor" | "weak" | "dislike" | "not"
            )
        })
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
