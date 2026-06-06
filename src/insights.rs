//! In-process insight aggregation over captured observability events.
//!
//! Where [`crate::EventQuery`] is a flat predicate/projection layer,
//! [`Insights`] derives the rollups an analytics or debugging workflow
//! actually keys on — latency percentiles, token/cost totals, error rates
//! by [`crate::ErrorClass`], tool outcome counts — and reconstructs tool
//! lifecycle spans by pairing `tool.invoked` (and `tool.hosted_invoked`)
//! with their terminal event via the stable `call_id` correlator.
//!
//! Everything here is computed from a borrowed snapshot of
//! [`ObservabilityEvent`] values, so it is allocation-light, host-owned, and
//! intended for tests, demos, and small local dashboards. Production
//! deployments should keep aggregating in their existing metrics backend off
//! the `rig_tap.*` tracing scalars; this module is the in-process companion
//! to [`crate::EventQuery`], not a replacement for a TSDB.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::event::{ErrorClass, EventKind, ObservabilityEvent};
use crate::query::EventQuery;

/// Summary statistics over a set of millisecond latency samples.
///
/// Percentiles use the nearest-rank method on the sorted sample set. All
/// fields are `None` when no samples were observed (`count == 0`).
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatencySummary {
    /// Number of latency samples observed.
    pub count: usize,
    /// Smallest sample, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_ms: Option<u64>,
    /// Largest sample, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_ms: Option<u64>,
    /// Arithmetic mean (integer-truncated), if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mean_ms: Option<u64>,
    /// 50th percentile (median), if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p50_ms: Option<u64>,
    /// 90th percentile, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p90_ms: Option<u64>,
    /// 95th percentile, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p95_ms: Option<u64>,
    /// 99th percentile, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p99_ms: Option<u64>,
}

impl LatencySummary {
    /// Build a summary from raw millisecond samples. The input is sorted
    /// internally; sample order does not matter.
    pub fn from_samples(mut samples: Vec<u64>) -> Self {
        samples.sort_unstable();
        let count = samples.len();
        if count == 0 {
            return Self::default();
        }
        let sum: u128 = samples.iter().map(|&value| u128::from(value)).sum();
        let mean = (sum / count as u128) as u64;
        Self {
            count,
            min_ms: samples.first().copied(),
            max_ms: samples.last().copied(),
            mean_ms: Some(mean),
            p50_ms: percentile(&samples, 50.0),
            p90_ms: percentile(&samples, 90.0),
            p95_ms: percentile(&samples, 95.0),
            p99_ms: percentile(&samples, 99.0),
        }
    }

    /// Return `true` when no samples contributed to this summary.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

/// Nearest-rank percentile over an already-sorted ascending slice.
fn percentile(sorted: &[u64], p: f64) -> Option<u64> {
    let len = sorted.len();
    if len == 0 {
        return None;
    }
    let rank = (p / 100.0 * len as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(len - 1);
    sorted.get(idx).copied()
}

/// Provider-reported token totals summed across `prompt.completed` events.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenTotals {
    /// Sum of `tokens_in` across completed prompts.
    pub tokens_in: u64,
    /// Sum of `tokens_out` across completed prompts.
    pub tokens_out: u64,
    /// Sum of `cached_tokens_in` across completed prompts.
    pub cached_tokens_in: u64,
    /// Sum of `reasoning_tokens` across completed prompts.
    pub reasoning_tokens: u64,
}

/// Aggregated counts and economics over the prompt lifecycle.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptStats {
    /// Number of `prompt.started` events.
    pub started: usize,
    /// Number of `prompt.completed` events.
    pub completed: usize,
    /// Number of `prompt.failed` events.
    pub failed: usize,
    /// Token totals summed across `prompt.completed`.
    pub tokens: TokenTotals,
    /// Sum of producer-computed `cost_usd` across `prompt.completed`.
    pub total_cost_usd: f64,
    /// Histogram of `finish_reason` values across `prompt.completed`.
    pub finish_reasons: BTreeMap<String, usize>,
}

impl PromptStats {
    /// Fraction of resolved prompts that completed successfully
    /// (`completed / (completed + failed)`). `None` when no prompt resolved.
    pub fn success_rate(&self) -> Option<f64> {
        let resolved = self.completed + self.failed;
        if resolved == 0 {
            return None;
        }
        Some(self.completed as f64 / resolved as f64)
    }
}

/// Aggregated outcome counts over the tool lifecycle (both agent-loop and
/// provider-hosted tools).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolStats {
    /// Number of `tool.invoked` + `tool.hosted_invoked` events.
    pub invoked: usize,
    /// Number of `tool.completed` + `tool.hosted_completed` events.
    pub completed: usize,
    /// Number of `tool.failed` events.
    pub failed: usize,
    /// Number of `tool.skipped` events.
    pub skipped: usize,
    /// Number of `tool.terminated` events.
    pub terminated: usize,
    /// Invocations with no observed terminal event in the snapshot.
    pub pending: usize,
}

impl ToolStats {
    /// Fraction of resolved invocations that completed successfully
    /// (`completed / (completed + failed + skipped + terminated)`).
    /// `None` when nothing resolved.
    pub fn success_rate(&self) -> Option<f64> {
        let resolved = self.completed + self.failed + self.skipped + self.terminated;
        if resolved == 0 {
            return None;
        }
        Some(self.completed as f64 / resolved as f64)
    }
}

/// Terminal disposition of a reconstructed [`ToolSpan`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ToolOutcome {
    /// The invocation has no terminal event in the snapshot.
    Pending,
    /// Paired with `tool.completed` / `tool.hosted_completed`.
    Completed,
    /// Paired with `tool.failed`, carrying the failure classification.
    Failed {
        /// Failure classification from the paired `tool.failed`.
        error_class: ErrorClass,
    },
    /// Paired with `tool.skipped`.
    Skipped,
    /// Paired with `tool.terminated`.
    Terminated,
}

/// A reconstructed tool-call span: a `tool.invoked` (or
/// `tool.hosted_invoked`) paired with its terminal event by `call_id`.
///
/// `duration_ms` prefers a producer-stamped duration from the terminal
/// event; when absent it is derived from the wall-clock delta between the
/// invoke and terminal `occurred_at_millis` (skew-safe via checked
/// subtraction). Spans whose invoke had no terminal in the snapshot carry
/// [`ToolOutcome::Pending`] and a `None` duration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpan {
    /// Conversation the span belongs to.
    pub conversation_id: String,
    /// Tool name from the paired invoke.
    pub tool_name: String,
    /// Stable correlation id shared by the invoke and its terminal.
    pub call_id: String,
    /// `true` when reconstructed from `tool.hosted_*` events.
    pub hosted: bool,
    /// Monotonic `tick` of the invoke event.
    pub start_tick: u64,
    /// Monotonic `tick` of the terminal event, if observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_tick: Option<u64>,
    /// Terminal disposition.
    #[serde(flatten)]
    pub outcome: ToolOutcome,
    /// Span duration in milliseconds, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// Derived insight view over a borrowed snapshot of observability events.
///
/// Construct via [`EventQuery::insights`]. All methods recompute from the
/// borrowed slice on each call; cache the results if you call them in a hot
/// loop.
#[derive(Debug, Clone, Copy)]
pub struct Insights<'a> {
    events: &'a [ObservabilityEvent],
}

impl<'a> Insights<'a> {
    /// Build an insight view over `events`.
    pub fn new(events: &'a [ObservabilityEvent]) -> Self {
        Self { events }
    }

    /// Aggregate prompt-lifecycle counts, token totals, cost, and
    /// `finish_reason` distribution.
    pub fn prompt_stats(&self) -> PromptStats {
        let mut stats = PromptStats::default();
        for event in self.events {
            match &event.kind {
                EventKind::PromptStarted { .. } => stats.started += 1,
                EventKind::PromptCompleted {
                    tokens_in,
                    tokens_out,
                    cached_tokens_in,
                    reasoning_tokens,
                    cost_usd,
                    finish_reason,
                    ..
                } => {
                    stats.completed += 1;
                    stats.tokens.tokens_in += tokens_in.unwrap_or(0);
                    stats.tokens.tokens_out += tokens_out.unwrap_or(0);
                    stats.tokens.cached_tokens_in += cached_tokens_in.unwrap_or(0);
                    stats.tokens.reasoning_tokens += reasoning_tokens.unwrap_or(0);
                    stats.total_cost_usd += cost_usd.unwrap_or(0.0);
                    if let Some(reason) = finish_reason {
                        *stats.finish_reasons.entry(reason.clone()).or_insert(0) += 1;
                    }
                }
                EventKind::PromptFailed { .. } => stats.failed += 1,
                _ => {}
            }
        }
        stats
    }

    /// Aggregate tool-lifecycle outcome counts across both agent-loop and
    /// provider-hosted tools. `pending` is the count of invocations with no
    /// matching terminal event in the snapshot.
    pub fn tool_stats(&self) -> ToolStats {
        let mut stats = ToolStats::default();
        for event in self.events {
            match &event.kind {
                EventKind::ToolInvoked { .. } | EventKind::ToolHostedInvoked { .. } => {
                    stats.invoked += 1;
                }
                EventKind::ToolCompleted { .. } | EventKind::ToolHostedCompleted { .. } => {
                    stats.completed += 1;
                }
                EventKind::ToolFailed { .. } => stats.failed += 1,
                EventKind::ToolSkipped { .. } => stats.skipped += 1,
                EventKind::ToolTerminated { .. } => stats.terminated += 1,
                _ => {}
            }
        }
        let resolved = stats.completed + stats.failed + stats.skipped + stats.terminated;
        stats.pending = stats.invoked.saturating_sub(resolved);
        stats
    }

    /// Reconstruct tool-call spans by pairing each invoke with its terminal
    /// event via `call_id`. Spans are returned in invoke order; invocations
    /// without a terminal carry [`ToolOutcome::Pending`].
    pub fn tool_spans(&self) -> Vec<ToolSpan> {
        let mut spans: Vec<ToolSpan> = Vec::new();
        // (conversation_id, call_id) -> index into `spans`.
        let mut index: BTreeMap<(String, String), usize> = BTreeMap::new();

        for event in self.events {
            let conversation_id = event.conversation_id.clone();
            match &event.kind {
                EventKind::ToolInvoked {
                    tool_name, call_id, ..
                } => {
                    open_span(
                        &mut spans,
                        &mut index,
                        conversation_id,
                        tool_name.clone(),
                        call_id.clone(),
                        false,
                        event.tick,
                    );
                }
                EventKind::ToolHostedInvoked {
                    tool_name, call_id, ..
                } => {
                    open_span(
                        &mut spans,
                        &mut index,
                        conversation_id,
                        tool_name.clone(),
                        call_id.clone(),
                        true,
                        event.tick,
                    );
                }
                EventKind::ToolCompleted {
                    call_id,
                    duration_ms,
                    ..
                }
                | EventKind::ToolHostedCompleted {
                    call_id,
                    duration_ms,
                    ..
                } => {
                    close_span(
                        &mut spans,
                        &index,
                        &conversation_id,
                        call_id,
                        event.tick,
                        *duration_ms,
                        ToolOutcome::Completed,
                        self.events,
                    );
                }
                EventKind::ToolFailed {
                    call_id,
                    error_class,
                    ..
                } => {
                    close_span(
                        &mut spans,
                        &index,
                        &conversation_id,
                        call_id,
                        event.tick,
                        None,
                        ToolOutcome::Failed {
                            error_class: *error_class,
                        },
                        self.events,
                    );
                }
                EventKind::ToolSkipped { call_id, .. } => {
                    close_span(
                        &mut spans,
                        &index,
                        &conversation_id,
                        call_id,
                        event.tick,
                        None,
                        ToolOutcome::Skipped,
                        self.events,
                    );
                }
                EventKind::ToolTerminated { call_id, .. } => {
                    close_span(
                        &mut spans,
                        &index,
                        &conversation_id,
                        call_id,
                        event.tick,
                        None,
                        ToolOutcome::Terminated,
                        self.events,
                    );
                }
                _ => {}
            }
        }
        spans
    }

    /// Latency summary over the `duration_ms` field of every event whose
    /// wire `kind` matches `kind` (e.g. `"tool.completed"`,
    /// `"prompt.completed"`, `"response.turn_completed"`).
    pub fn latency(&self, kind: &str) -> LatencySummary {
        let samples = self
            .events
            .iter()
            .filter(|event| event.kind.discriminant() == kind)
            .filter_map(|event| duration_ms_of(&event.kind))
            .collect();
        LatencySummary::from_samples(samples)
    }

    /// Latency summary over `time_to_first_token_ms` across
    /// `prompt.completed` events from streaming producers.
    pub fn time_to_first_token(&self) -> LatencySummary {
        let samples = self
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::PromptCompleted {
                    time_to_first_token_ms,
                    ..
                } => *time_to_first_token_ms,
                _ => None,
            })
            .collect();
        LatencySummary::from_samples(samples)
    }

    /// Count failures by [`ErrorClass`] across `prompt.failed` and
    /// `tool.failed` events. Keyed by the snake_case discriminant.
    pub fn errors_by_class(&self) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        for event in self.events {
            let class = match &event.kind {
                EventKind::PromptFailed { error_class, .. }
                | EventKind::ToolFailed { error_class, .. } => Some(error_class),
                _ => None,
            };
            if let Some(class) = class {
                *counts.entry(class.as_str().to_owned()).or_insert(0) += 1;
            }
        }
        counts
    }

    /// Sum producer-computed `cost_usd` from `prompt.completed`, grouped by
    /// `conversation_id`. Conversations with no reported cost are omitted.
    pub fn cost_by_conversation(&self) -> BTreeMap<String, f64> {
        let mut costs: BTreeMap<String, f64> = BTreeMap::new();
        for event in self.events {
            if let EventKind::PromptCompleted {
                cost_usd: Some(cost),
                ..
            } = &event.kind
            {
                *costs.entry(event.conversation_id.clone()).or_insert(0.0) += cost;
            }
        }
        costs
    }
}

/// Open a new pending span and record its index for later pairing.
fn open_span(
    spans: &mut Vec<ToolSpan>,
    index: &mut BTreeMap<(String, String), usize>,
    conversation_id: String,
    tool_name: String,
    call_id: String,
    hosted: bool,
    start_tick: u64,
) {
    let key = (conversation_id.clone(), call_id.clone());
    index.insert(key, spans.len());
    spans.push(ToolSpan {
        conversation_id,
        tool_name,
        call_id,
        hosted,
        start_tick,
        end_tick: None,
        outcome: ToolOutcome::Pending,
        duration_ms: None,
    });
}

/// Finalize the span matching `(conversation_id, call_id)`, if one is open.
/// Terminal events with no matching invoke are ignored (an orphaned terminal
/// has no start to anchor a span).
#[allow(clippy::too_many_arguments)]
fn close_span(
    spans: &mut [ToolSpan],
    index: &BTreeMap<(String, String), usize>,
    conversation_id: &str,
    call_id: &str,
    end_tick: u64,
    explicit_duration_ms: Option<u64>,
    outcome: ToolOutcome,
    events: &[ObservabilityEvent],
) {
    let key = (conversation_id.to_owned(), call_id.to_owned());
    let Some(&idx) = index.get(&key) else {
        return;
    };
    let Some(span) = spans.get_mut(idx) else {
        return;
    };
    span.end_tick = Some(end_tick);
    span.outcome = outcome;
    span.duration_ms = explicit_duration_ms.or_else(|| {
        let start = millis_at_tick(events, conversation_id, span.start_tick)?;
        let end = millis_at_tick(events, conversation_id, end_tick)?;
        end.checked_sub(start)
    });
}

/// Look up the `occurred_at_millis` of the event at `tick` in `conversation_id`.
fn millis_at_tick(events: &[ObservabilityEvent], conversation_id: &str, tick: u64) -> Option<u64> {
    events
        .iter()
        .find(|event| event.conversation_id == conversation_id && event.tick == tick)
        .map(|event| event.occurred_at_millis)
}

/// Extract the `duration_ms` carried by a terminal event kind, if any.
fn duration_ms_of(kind: &EventKind) -> Option<u64> {
    match kind {
        EventKind::PromptCompleted { duration_ms, .. }
        | EventKind::ToolCompleted { duration_ms, .. }
        | EventKind::ToolHostedCompleted { duration_ms, .. }
        | EventKind::ResponseTurnCompleted { duration_ms, .. } => *duration_ms,
        _ => None,
    }
}

impl EventQuery {
    /// Derive an [`Insights`] view over this snapshot's events.
    pub fn insights(&self) -> Insights<'_> {
        Insights::new(self.all())
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::expect_used
)]
mod tests {
    use super::*;
    use crate::event::SCHEMA_VERSION;

    fn event(tick: u64, conversation_id: &str, kind: EventKind) -> ObservabilityEvent {
        ObservabilityEvent {
            version: SCHEMA_VERSION,
            occurred_at_millis: 1_715_000_000_000 + tick,
            tick,
            conversation_id: conversation_id.into(),
            span_id: None,
            agent_id: None,
            trace_id: None,
            kind,
        }
    }

    fn invoked(tick: u64, conv: &str, call_id: &str) -> ObservabilityEvent {
        event(
            tick,
            conv,
            EventKind::ToolInvoked {
                tool_name: "search".into(),
                provider_call_id: None,
                call_id: call_id.into(),
                args_json: "{}".into(),
                truncated: false,
            },
        )
    }

    fn completed(
        tick: u64,
        conv: &str,
        call_id: &str,
        duration_ms: Option<u64>,
    ) -> ObservabilityEvent {
        event(
            tick,
            conv,
            EventKind::ToolCompleted {
                tool_name: "search".into(),
                provider_call_id: None,
                call_id: call_id.into(),
                result: "ok".into(),
                truncated: false,
                duration_ms,
            },
        )
    }

    #[test]
    fn latency_summary_percentiles_and_mean() {
        let summary = LatencySummary::from_samples(vec![10, 20, 30, 40, 100]);
        assert_eq!(summary.count, 5);
        assert_eq!(summary.min_ms, Some(10));
        assert_eq!(summary.max_ms, Some(100));
        assert_eq!(summary.mean_ms, Some(40));
        assert_eq!(summary.p50_ms, Some(30));
        assert_eq!(summary.p90_ms, Some(100));
        assert!(!summary.is_empty());
    }

    #[test]
    fn empty_latency_summary_is_empty() {
        let summary = LatencySummary::from_samples(vec![]);
        assert!(summary.is_empty());
        assert_eq!(summary.p50_ms, None);
    }

    #[test]
    fn prompt_stats_sums_tokens_cost_and_finish_reasons() {
        let query = EventQuery::new(vec![
            event(
                1,
                "a",
                EventKind::PromptStarted {
                    model: "m".into(),
                    messages_in: 1,
                },
            ),
            event(
                2,
                "a",
                EventKind::PromptCompleted {
                    model: "m".into(),
                    tokens_in: Some(100),
                    tokens_out: Some(50),
                    cached_tokens_in: Some(10),
                    reasoning_tokens: Some(5),
                    cost_usd: Some(0.25),
                    finish_reason: Some("stop".into()),
                    response_id: None,
                    previous_response_id: None,
                    time_to_first_token_ms: Some(120),
                    duration_ms: Some(800),
                },
            ),
            event(
                3,
                "a",
                EventKind::PromptFailed {
                    model: "m".into(),
                    error_class: ErrorClass::RateLimit,
                    message: "429".into(),
                    retriable: true,
                    provider_error_code: None,
                    http_status: Some(429),
                },
            ),
        ]);

        let insights = query.insights();
        let stats = insights.prompt_stats();
        assert_eq!(stats.started, 1);
        assert_eq!(stats.completed, 1);
        assert_eq!(stats.failed, 1);
        assert_eq!(stats.tokens.tokens_in, 100);
        assert_eq!(stats.tokens.tokens_out, 50);
        assert_eq!(stats.tokens.cached_tokens_in, 10);
        assert_eq!(stats.tokens.reasoning_tokens, 5);
        assert!((stats.total_cost_usd - 0.25).abs() < f64::EPSILON);
        assert_eq!(stats.finish_reasons.get("stop"), Some(&1));
        assert_eq!(stats.success_rate(), Some(0.5));

        assert_eq!(insights.time_to_first_token().p50_ms, Some(120));
        assert_eq!(insights.latency("prompt.completed").p50_ms, Some(800));
        assert_eq!(insights.errors_by_class().get("rate_limit"), Some(&1));
        assert!(
            (insights.cost_by_conversation().get("a").copied().unwrap() - 0.25).abs()
                < f64::EPSILON
        );
    }

    #[test]
    fn tool_spans_pair_invoke_and_terminal_with_derived_duration() {
        let query = EventQuery::new(vec![
            invoked(1, "a", "call-1"),
            completed(5, "a", "call-1", None),
            invoked(2, "a", "call-2"),
            event(
                6,
                "a",
                EventKind::ToolFailed {
                    tool_name: "search".into(),
                    call_id: "call-2".into(),
                    error_class: ErrorClass::Timeout,
                    message: "slow".into(),
                },
            ),
            invoked(3, "a", "call-3"), // pending, no terminal
        ]);

        let insights = query.insights();
        let spans = insights.tool_spans();
        assert_eq!(spans.len(), 3);

        let first = &spans[0];
        assert_eq!(first.call_id, "call-1");
        assert_eq!(first.outcome, ToolOutcome::Completed);
        assert_eq!(first.duration_ms, Some(4)); // derived from millis delta (5 - 1)

        let second = &spans[1];
        assert_eq!(second.call_id, "call-2");
        assert_eq!(
            second.outcome,
            ToolOutcome::Failed {
                error_class: ErrorClass::Timeout
            }
        );

        let third = &spans[2];
        assert_eq!(third.call_id, "call-3");
        assert_eq!(third.outcome, ToolOutcome::Pending);
        assert_eq!(third.end_tick, None);
        assert_eq!(third.duration_ms, None);
    }

    #[test]
    fn tool_completed_explicit_duration_wins_over_derived() {
        let query = EventQuery::new(vec![
            invoked(1, "a", "call-1"),
            completed(5, "a", "call-1", Some(999)),
        ]);
        let spans = query.insights().tool_spans();
        assert_eq!(spans[0].duration_ms, Some(999));
    }

    #[test]
    fn tool_stats_counts_outcomes_and_pending() {
        let query = EventQuery::new(vec![
            invoked(1, "a", "call-1"),
            completed(2, "a", "call-1", None),
            invoked(3, "a", "call-2"),
            event(
                4,
                "a",
                EventKind::ToolSkipped {
                    tool_name: "search".into(),
                    call_id: "call-2".into(),
                    reason: "gated".into(),
                },
            ),
            invoked(5, "a", "call-3"), // pending
        ]);

        let stats = query.insights().tool_stats();
        assert_eq!(stats.invoked, 3);
        assert_eq!(stats.completed, 1);
        assert_eq!(stats.skipped, 1);
        assert_eq!(stats.pending, 1);
        assert_eq!(stats.success_rate(), Some(0.5));
    }
}
