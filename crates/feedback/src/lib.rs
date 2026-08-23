// SPDX-License-Identifier: Apache-2.0
//! Feedback persistence (ADR-0018): the decision journal and accepted
//! feedback as two append-only JSONL streams, joined on `decisionId` by the
//! offline trainer.
//!
//! Prompts are never logged or persisted: a [`DecisionRecord`] carries only
//! closed-vocabulary findings (task, complexity, risk, PII entity names) and
//! routing facts. Writes go through a bounded channel to a writer task; the
//! hot path never blocks on disk, and records are dropped (and counted)
//! rather than awaited when the channel is full.

use std::path::Path;

use routed_decision::Decision;
use serde::{Deserialize, Serialize};

/// One line of `decisions.jsonl`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionRecord {
    /// Decision id (ULID); the join key.
    pub decision_id: String,
    /// UTC timestamp in RFC 3339 (seconds).
    pub ts: String,
    /// `inline`, `extproc`, `decide` or `dry-run`.
    pub mode: String,
    /// Outcome.
    pub outcome: String,
    /// Matched policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<String>,
    /// Requested model alias.
    pub requested_model: String,
    /// Selected tier (`ROUTE`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_tier: Option<String>,
    /// Gateway model (`ROUTE`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_model: Option<String>,
    /// Effective data class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_class: Option<String>,
    /// Task label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// Complexity label (`low` / `medium` / `high`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub complexity: Option<String>,
    /// Risk score.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_score: Option<f64>,
    /// PII entity names (closed vocabulary).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pii_entities: Vec<String>,
    /// Estimated cost in EUR.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cost_eur: Option<f64>,
    /// Snapshot the decision was made against.
    pub snapshot_hash: String,
}

/// A serde-renamed enum's canonical JSON label (the same string the
/// Decision JSON uses).
fn label<T: Serialize>(v: T) -> Option<String> {
    serde_json::to_value(v)
        .ok()
        .and_then(|j| j.as_str().map(ToOwned::to_owned))
}

impl DecisionRecord {
    /// Build a record from a decision (no request content involved).
    #[must_use]
    pub fn from_decision(d: &Decision, mode: &str, ts: String) -> Self {
        Self {
            decision_id: d.id.clone(),
            ts,
            mode: if d.dry_run {
                "dry-run".into()
            } else {
                mode.into()
            },
            outcome: d.outcome.to_string(),
            policy: d.policy.clone(),
            requested_model: d.requested_model.clone(),
            selected_tier: d.selected_tier.clone(),
            gateway_model: d.gateway_model.clone(),
            data_class: d.data_class.clone(),
            task: d.classification.task.clone(),
            complexity: d.classification.complexity.and_then(label),
            risk_score: d.classification.risk_score,
            pii_entities: d
                .classification
                .pii_entities
                .iter()
                .filter_map(|e| label(*e))
                .collect(),
            estimated_cost_eur: d.estimated_cost_eur,
            snapshot_hash: d.snapshot_hash.clone(),
        }
    }
}

/// One line of `feedback.jsonl`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackRecord {
    /// Decision id (ULID); the join key.
    pub decision_id: String,
    /// UTC timestamp in RFC 3339 (seconds).
    pub ts: String,
    /// `user`, `agent`, `gateway` or `unknown`.
    pub source: String,
    /// The caller's outcome object, as sent (bounded by the handler).
    pub outcome: serde_json::Value,
}

/// Where records go (ADR-0004 seam). Implementations must be cheap to call
/// from the request path.
pub trait FeedbackSink: Send + Sync {
    /// Record a made decision.
    fn record_decision(&self, record: DecisionRecord);
    /// Record accepted feedback.
    fn record_feedback(&self, record: FeedbackRecord);
}

/// Discards everything (the default when `--feedback-dir` is unset).
#[derive(Clone, Copy, Debug, Default)]
pub struct NullSink;

impl FeedbackSink for NullSink {
    fn record_decision(&self, _record: DecisionRecord) {}
    fn record_feedback(&self, _record: FeedbackRecord) {}
}

enum Line {
    Decision(String),
    Feedback(String),
}

/// Appends `decisions.jsonl` / `feedback.jsonl` under a directory.
pub struct JsonlSink {
    tx: tokio::sync::mpsc::Sender<Line>,
}

impl JsonlSink {
    /// Spawn the writer task. The directory is created if missing.
    ///
    /// # Errors
    /// When the directory cannot be created or the files cannot be opened.
    pub fn spawn(dir: &Path) -> std::io::Result<Self> {
        use std::io::Write as _;
        std::fs::create_dir_all(dir)?;
        let open = |name: &str| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join(name))
        };
        let mut decisions = open("decisions.jsonl")?;
        let mut feedback = open("feedback.jsonl")?;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Line>(1024);
        tokio::spawn(async move {
            while let Some(line) = rx.recv().await {
                let result = match line {
                    Line::Decision(l) => writeln!(decisions, "{l}"),
                    Line::Feedback(l) => writeln!(feedback, "{l}"),
                };
                if let Err(e) = result {
                    tracing::warn!(error = %e, "feedback sink write failed");
                }
            }
        });
        Ok(Self { tx })
    }

    fn send(&self, line: Line) {
        // try_send: a full channel drops the record instead of blocking the
        // request path (ADR-0018).
        if self.tx.try_send(line).is_err() {
            tracing::warn!("feedback sink channel full; record dropped");
        }
    }
}

impl FeedbackSink for JsonlSink {
    fn record_decision(&self, record: DecisionRecord) {
        match serde_json::to_string(&record) {
            Ok(l) => self.send(Line::Decision(l)),
            Err(e) => tracing::warn!(error = %e, "unserialisable decision record"),
        }
    }

    fn record_feedback(&self, record: FeedbackRecord) {
        match serde_json::to_string(&record) {
            Ok(l) => self.send(Line::Feedback(l)),
            Err(e) => tracing::warn!(error = %e, "unserialisable feedback record"),
        }
    }
}

/// UTC now in RFC 3339 with second precision (no chrono dependency).
#[must_use]
pub fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Days-to-date conversion (proleptic Gregorian), valid for 1970..9999.
    let days = secs / 86_400;
    let (mut y, mut rem) = (1970u64, days);
    loop {
        let leap = u64::from(y % 4 == 0 && (y % 100 != 0 || y % 400 == 0));
        let len = 365 + leap;
        if rem < len {
            break;
        }
        rem -= len;
        y += 1;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let months = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut m = 0;
    while rem >= months[m] {
        rem -= months[m];
        m += 1;
    }
    let (h, min, s) = (secs / 3600 % 24, secs / 60 % 60, secs % 60);
    format!("{y:04}-{:02}-{:02}T{h:02}:{min:02}:{s:02}Z", m + 1, rem + 1)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_shape_and_epoch() {
        let s = now_rfc3339();
        assert_eq!(s.len(), 20, "{s}");
        assert!(s.ends_with('Z') && s.contains('T'));
        assert!(s.as_str() >= "2026-01-01T00:00:00Z", "{s}");
    }

    #[tokio::test]
    async fn jsonl_sink_writes_both_streams() {
        let dir = std::env::temp_dir().join(format!("routed-feedback-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sink = JsonlSink::spawn(&dir).unwrap();
        sink.record_feedback(FeedbackRecord {
            decision_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
            ts: now_rfc3339(),
            source: "agent".into(),
            outcome: serde_json::json!({ "success": true, "rating": 4 }),
        });
        // The writer task is async; poll for the line.
        let path = dir.join("feedback.jsonl");
        let mut content = String::new();
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            content = std::fs::read_to_string(&path).unwrap_or_default();
            if !content.is_empty() {
                break;
            }
        }
        let v: serde_json::Value = serde_json::from_str(content.lines().next().unwrap()).unwrap();
        assert_eq!(v["decisionId"], "01ARZ3NDEKTSV4RRFFQ69G5FAV");
        assert_eq!(v["outcome"]["rating"], 4);
        assert!(dir.join("decisions.jsonl").exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
