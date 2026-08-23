// SPDX-License-Identifier: Apache-2.0
//! Classification with timeout + bounded concurrency, and the decision step.

use std::sync::Arc;
use std::time::{Duration, Instant};

use routed_classify::{Classifier, ClassifyError, ClassifyInput};
use routed_decision::Findings;
use routed_router::degraded_name;
use routed_telemetry::{ClassifierErrorLabels, NameLabel, Telemetry};
use tokio::sync::Semaphore;

/// Runs a synchronous classifier on the blocking pool under a deadline.
pub struct ClassifyRunner {
    /// Classifier.
    pub classifier: Arc<dyn Classifier>,
    /// Concurrency limit.
    pub semaphore: Arc<Semaphore>,
    /// Deadline per call (queueing included).
    pub timeout: Duration,
    /// Telemetry.
    pub telemetry: Arc<Telemetry>,
}

impl ClassifyRunner {
    /// Classify; errors and timeouts become degraded findings (ADR-0006).
    pub async fn run(&self, input: ClassifyInput) -> Findings {
        let name = self.classifier.name();
        let started = Instant::now();
        let classifier = Arc::clone(&self.classifier);
        let permit =
            tokio::time::timeout(self.timeout, Arc::clone(&self.semaphore).acquire_owned()).await;
        let Ok(Ok(permit)) = permit else {
            return self.degraded(name, &ClassifyError::Timeout(name.into()), started);
        };
        let remaining = self.timeout.saturating_sub(started.elapsed());
        // The permit travels with the blocking task so a timed-out classifier keeps
        // its slot until it actually finishes (bounded blocking-pool usage).
        let task = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            classifier.classify(&input)
        });
        match tokio::time::timeout(remaining, task).await {
            Ok(Ok(Ok(findings))) => {
                self.telemetry
                    .metrics
                    .classifier_latency
                    .get_or_create(&NameLabel { name: name.into() })
                    .observe(started.elapsed().as_secs_f64());
                findings
            }
            Ok(Ok(Err(e))) => self.degraded(name, &e, started),
            Ok(Err(join)) => self.degraded(
                name,
                &ClassifyError::Failed(format!("classifier panicked: {join}")),
                started,
            ),
            Err(_) => self.degraded(name, &ClassifyError::Timeout(name.into()), started),
        }
    }

    fn degraded(&self, name: &str, e: &ClassifyError, started: Instant) -> Findings {
        let kind = match e {
            ClassifyError::Timeout(_) => "timeout",
            ClassifyError::Unavailable(_) => "unavailable",
            ClassifyError::Failed(_) => "failed",
        };
        tracing::warn!(classifier = name, error = %e, elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX), "classification degraded");
        self.telemetry
            .metrics
            .classifier_errors_total
            .get_or_create(&ClassifierErrorLabels {
                classifier: name.into(),
                kind: kind.into(),
            })
            .inc();
        Findings {
            degraded: vec![degraded_name(name, e)],
            ..Default::default()
        }
    }
}
