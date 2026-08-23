// SPDX-License-Identifier: Apache-2.0
//! ONNX classifier (feature `onnx`, ADR-0016): one multi-head encoder
//! producing task, complexity, sensitivity and risk findings.
//!
//! The model can only ever tighten what the heuristics already catch: PII
//! entities always come from `routed-security`'s span detectors, and the
//! reported risk score is `max(model risk, heuristic injection score)`.

use std::path::Path;
use std::sync::Mutex;

use ort::session::Session;
use ort::value::Tensor;
use routed_decision::{Complexity, Findings};
use routed_security::{detect_pii, score_injection};
use tokenizers::Tokenizer;

use crate::{Classifier, ClassifyError, ClassifyInput};

/// Output head names (the trainer's export contract, ADR-0016).
const TASK_HEAD: &str = "task_logits";
const COMPLEXITY_HEAD: &str = "complexity_logits";
const SENSITIVITY_HEAD: &str = "sensitivity_logits";
const RISK_HEAD: &str = "risk";

/// ONNX-backed classifier.
pub struct OnnxClassifier {
    // `Session::run` needs `&mut self`; the router already bounds classifier
    // concurrency with a semaphore, so one serialized session is acceptable.
    session: Mutex<Session>,
    tokenizer: Tokenizer,
    task_labels: Vec<String>,
    sensitivity_labels: Vec<String>,
    max_chars: usize,
}

impl OnnxClassifier {
    /// Load a model and tokenizer from local files (already resolved and
    /// digest-verified by `routed-artifact`).
    ///
    /// # Errors
    /// `ClassifyError::Unavailable` when either artifact fails to load or the
    /// model lacks the required `risk` head.
    pub fn load(
        model: &Path,
        tokenizer: &Path,
        task_labels: Vec<String>,
        sensitivity_labels: Vec<String>,
    ) -> Result<Self, ClassifyError> {
        let unavailable = |what: &str, e: String| {
            ClassifyError::Unavailable(format!("onnx classifier: {what}: {e}"))
        };
        let session = Session::builder()
            .and_then(|mut b| b.commit_from_file(model))
            .map_err(|e| unavailable("model", e.to_string()))?;
        if !session.outputs().iter().any(|o| o.name() == RISK_HEAD) {
            return Err(ClassifyError::Unavailable(format!(
                "onnx classifier: model has no `{RISK_HEAD}` output (ADR-0016)"
            )));
        }
        let tokenizer =
            Tokenizer::from_file(tokenizer).map_err(|e| unavailable("tokenizer", e.to_string()))?;
        Ok(Self {
            session: Mutex::new(session),
            tokenizer,
            task_labels,
            sensitivity_labels,
            max_chars: 32_000,
        })
    }

    /// Model heads for `text`: `(task_idx, complexity_idx, sensitivity_idx, risk)`.
    #[allow(clippy::type_complexity)]
    fn run_model(
        &self,
        text: &str,
    ) -> Result<(Option<usize>, Option<usize>, Option<usize>, f64), ClassifyError> {
        let failed =
            |what: &str, e: String| ClassifyError::Failed(format!("onnx classifier: {what}: {e}"));
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| failed("tokenize", e.to_string()))?;
        let ids: Vec<i64> = encoding.get_ids().iter().map(|&u| i64::from(u)).collect();
        let mask: Vec<i64> = encoding
            .get_attention_mask()
            .iter()
            .map(|&u| i64::from(u))
            .collect();
        if ids.is_empty() {
            // Nothing to embed; the caller falls back to heuristics alone.
            return Ok((None, None, None, 0.0));
        }
        let seq = i64::try_from(ids.len()).map_err(|e| failed("length", e.to_string()))?;
        let input_ids = Tensor::from_array((vec![1, seq], ids))
            .map_err(|e| failed("input tensor", e.to_string()))?;
        let attention = Tensor::from_array((vec![1, seq], mask))
            .map_err(|e| failed("mask tensor", e.to_string()))?;

        let mut session = self
            .session
            .lock()
            .map_err(|_| ClassifyError::Failed("onnx classifier: session poisoned".into()))?;
        let outputs = session
            .run(ort::inputs!["input_ids" => input_ids, "attention_mask" => attention])
            .map_err(|e| failed("run", e.to_string()))?;

        let head = |name: &str| -> Result<Option<Vec<f32>>, ClassifyError> {
            outputs.get(name).map_or(Ok(None), |v| {
                v.try_extract_tensor::<f32>()
                    .map(|(_, data)| Some(data.to_vec()))
                    .map_err(|e| failed(name, e.to_string()))
            })
        };
        let argmax = |logits: &Option<Vec<f32>>| {
            logits.as_ref().and_then(|l| {
                l.iter()
                    .enumerate()
                    .max_by(|a, b| a.1.total_cmp(b.1))
                    .map(|(i, _)| i)
            })
        };

        let task = argmax(&head(TASK_HEAD)?);
        let complexity = argmax(&head(COMPLEXITY_HEAD)?);
        let sensitivity = argmax(&head(SENSITIVITY_HEAD)?);
        let risk = head(RISK_HEAD)?
            .and_then(|r| r.first().copied())
            .ok_or_else(|| {
                ClassifyError::Failed("onnx classifier: `risk` head produced no value".into())
            })?;
        Ok((
            task,
            complexity,
            sensitivity,
            f64::from(risk).clamp(0.0, 1.0),
        ))
    }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

impl Classifier for OnnxClassifier {
    fn name(&self) -> &'static str {
        "onnx"
    }

    fn classify(&self, input: &ClassifyInput) -> Result<Findings, ClassifyError> {
        // Same text selection as every implementation (ADR-0006).
        let user = truncate(&input.user_text, self.max_chars);
        let mut model_text = String::new();
        if let Some(sp) = &input.system_prompt {
            model_text.push_str(truncate(sp, self.max_chars));
            model_text.push('\n');
        }
        model_text.push_str(user);
        for t in &input.tool_outputs {
            model_text.push('\n');
            model_text.push_str(truncate(t, self.max_chars));
        }

        let (task_idx, complexity_idx, sensitivity_idx, model_risk) =
            self.run_model(&model_text)?;

        let mut findings = Findings {
            task: task_idx.and_then(|i| self.task_labels.get(i).cloned()),
            complexity: complexity_idx.map(|i| match i {
                0 => Complexity::Low,
                1 => Complexity::Medium,
                _ => Complexity::High,
            }),
            inferred_data_class: sensitivity_idx
                .and_then(|i| self.sensitivity_labels.get(i).cloned()),
            ..Default::default()
        };

        // Heuristics floor the model: span-level PII always comes from the
        // detectors, and risk can only be tightened by the model.
        let mut risk = model_risk;
        let mut injection_texts: Vec<&str> = vec![user];
        if let Some(sp) = &input.system_prompt {
            injection_texts.push(truncate(sp, self.max_chars));
        }
        for t in &input.tool_outputs {
            injection_texts.push(truncate(t, self.max_chars));
        }
        let mut pii_texts = injection_texts.clone();
        for t in &input.history {
            pii_texts.push(truncate(t, self.max_chars));
        }
        for t in &injection_texts {
            let (s, _) = score_injection(t);
            risk = risk.max(s);
        }
        for t in &pii_texts {
            for m in detect_pii(t) {
                findings.pii_entities.insert(m.entity);
                let e = findings.pii_confidence.entry(m.entity).or_insert(0.0);
                if m.confidence > *e {
                    *e = m.confidence;
                }
            }
        }
        findings.risk_score = Some(risk);
        Ok(findings)
    }
}

/// ONNX learned-router quality predictor (ADR-0018): `features` in,
/// `quality` (and optional `confidence`) out, plugged into the pure engine's
/// [`routed_decision::QualityPredictor`] seam. A prediction can only refine
/// `predictedQuality`; hard constraints are untouched.
pub struct OnnxQualityPredictor {
    session: Mutex<Session>,
    task_labels: Vec<String>,
}

impl OnnxQualityPredictor {
    /// Load the model (already resolved and digest-verified).
    ///
    /// # Errors
    /// `ClassifyError::Unavailable` when the model fails to load or lacks a
    /// `quality` output.
    pub fn load(model: &Path, task_labels: Vec<String>) -> Result<Self, ClassifyError> {
        let session = Session::builder()
            .and_then(|mut b| b.commit_from_file(model))
            .map_err(|e| ClassifyError::Unavailable(format!("learned router: model: {e}")))?;
        if !session.outputs().iter().any(|o| o.name() == "quality") {
            return Err(ClassifyError::Unavailable(
                "learned router: model has no `quality` output (ADR-0018)".into(),
            ));
        }
        Ok(Self {
            session: Mutex::new(session),
            task_labels,
        })
    }
}

impl routed_decision::QualityPredictor for OnnxQualityPredictor {
    fn predict(
        &self,
        tier: &routed_snapshot::CompiledTier,
        findings: &Findings,
    ) -> Option<(f64, f64)> {
        let features = crate::router_features::router_features(&self.task_labels, findings, tier);
        let n = i64::try_from(features.len()).ok()?;
        let input = Tensor::from_array((vec![1, n], features)).ok()?;
        let mut session = self.session.lock().ok()?;
        let outputs = session.run(ort::inputs!["features" => input]).ok()?;
        let scalar = |name: &str| {
            outputs
                .get(name)
                .and_then(|v| v.try_extract_tensor::<f32>().ok())
                .and_then(|(_, data)| data.first().copied())
                .map(f64::from)
        };
        let quality = scalar("quality")?.clamp(0.0, 1.0);
        let confidence = scalar("confidence").unwrap_or(1.0).clamp(0.0, 1.0);
        Some((quality, confidence))
    }
}
