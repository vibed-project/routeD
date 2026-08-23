// SPDX-License-Identifier: Apache-2.0
//! `type: http` classifier: an external service implementing the classifier
//! contract (`docs/classifier-http.md`). Synchronous (`ureq`) because it runs
//! on the blocking pool under the router's timeout.

use std::time::Duration;

use routed_decision::Findings;
use serde::Serialize;

use crate::{Classifier, ClassifyError, ClassifyInput};

/// HTTP classifier.
#[derive(Clone, Debug)]
pub struct HttpClassifier {
    /// Endpoint receiving `POST` with the JSON request below.
    pub endpoint: String,
    /// Request timeout.
    pub timeout: Duration,
    /// Optional bearer token.
    pub bearer: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Wire<'a> {
    system_prompt: Option<&'a str>,
    user_text: &'a str,
    history: &'a [String],
    tool_outputs: &'a [String],
}

impl Classifier for HttpClassifier {
    fn name(&self) -> &'static str {
        "http"
    }

    fn classify(&self, input: &ClassifyInput) -> Result<Findings, ClassifyError> {
        let agent = ureq::Agent::new_with_config(
            ureq::config::Config::builder()
                .timeout_global(Some(self.timeout))
                .build(),
        );
        let body = serde_json::to_string(&Wire {
            system_prompt: input.system_prompt.as_deref(),
            user_text: &input.user_text,
            history: &input.history,
            tool_outputs: &input.tool_outputs,
        })
        .map_err(|e| ClassifyError::Failed(e.to_string()))?;
        let mut req = agent
            .post(&self.endpoint)
            .header("content-type", "application/json");
        if let Some(b) = &self.bearer {
            req = req.header("authorization", &format!("Bearer {b}"));
        }
        let mut resp = req.send(body.as_bytes()).map_err(|e| match e {
            ureq::Error::Timeout(_) => ClassifyError::Timeout("http".into()),
            other => ClassifyError::Failed(other.to_string()),
        })?;
        if resp.status() != 200 {
            return Err(ClassifyError::Failed(format!(
                "classifier returned HTTP {}",
                resp.status()
            )));
        }
        let text = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| ClassifyError::Failed(e.to_string()))?;
        let findings: Findings = serde_json::from_str(&text)
            .map_err(|e| ClassifyError::Failed(format!("invalid findings JSON: {e}")))?;
        if findings
            .risk_score
            .is_some_and(|r| !(0.0..=1.0).contains(&r))
        {
            return Err(ClassifyError::Failed("riskScore out of range".into()));
        }
        Ok(findings)
    }
}
