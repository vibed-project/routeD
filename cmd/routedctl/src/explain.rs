// SPDX-License-Identifier: Apache-2.0
//! `routedctl explain`: run the full pipeline offline for one request and
//! print the decision plus the elimination trace.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::Context;
use routed_classify::{Classifier, StubClassifier};
use routed_decision::{Decision, DecisionContext, DecisionInput, Engine, Findings, Outcome};
use routed_policy::compile;
use routed_router::Pipeline;
use serde::Deserialize;

/// Optional `overrides.json` next to a request: partial engine input.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Overrides {
    /// Override the estimated input tokens.
    pub estimated_input_tokens: Option<u64>,
    /// Override the estimated output tokens.
    pub estimated_output_tokens: Option<u64>,
}

/// Inputs of one explanation.
#[derive(Clone, Debug)]
pub struct ExplainRequest {
    /// Resource files / directories.
    pub policy: Vec<PathBuf>,
    /// Request JSON body.
    pub request: Vec<u8>,
    /// Request path.
    pub path: String,
    /// Headers.
    pub headers: BTreeMap<String, String>,
    /// Findings to use instead of the heuristic classifier.
    pub findings: Option<Findings>,
    /// Partial input overrides.
    pub overrides: Overrides,
    /// Decision id (fixed in tests).
    pub id: String,
}

impl ExplainRequest {
    /// Build from an example directory: `request.json`, optional `headers.json`,
    /// `findings.json`, `overrides.json`, and `resources.yaml` (or `--policy`).
    ///
    /// # Errors
    /// On missing / malformed files.
    pub fn from_dir(dir: &Path, id: &str) -> anyhow::Result<Self> {
        let read_json = |name: &str| -> anyhow::Result<Option<String>> {
            let p = dir.join(name);
            if p.exists() {
                Ok(Some(
                    std::fs::read_to_string(&p)
                        .with_context(|| format!("reading {}", p.display()))?,
                ))
            } else {
                Ok(None)
            }
        };
        let request = std::fs::read(dir.join("request.json"))
            .with_context(|| format!("reading {}", dir.join("request.json").display()))?;
        let headers: BTreeMap<String, String> = read_json("headers.json")?
            .map(|s| serde_json::from_str(&s))
            .transpose()
            .context("headers.json")?
            .unwrap_or_default();
        let findings: Option<Findings> = read_json("findings.json")?
            .map(|s| serde_json::from_str(&s))
            .transpose()
            .context("findings.json")?;
        let overrides: Overrides = read_json("overrides.json")?
            .map(|s| serde_json::from_str(&s))
            .transpose()
            .context("overrides.json")?
            .unwrap_or_default();
        let path = read_json("path.txt")?
            .map_or_else(|| "/v1/chat/completions".into(), |s| s.trim().to_owned());
        let resources = dir.join("resources.yaml");
        let policy = if resources.exists() {
            vec![resources]
        } else {
            vec![
                dir.parent()
                    .map_or_else(|| PathBuf::from("."), |p| p.join("_shared")),
            ]
        };
        Ok(Self {
            policy,
            request,
            path,
            headers,
            findings,
            overrides,
            id: id.to_owned(),
        })
    }
}

/// Result of an explanation.
#[derive(Clone, Debug)]
pub struct Explanation {
    /// The decision.
    pub decision: Decision,
    /// Engine input.
    pub input: DecisionInput,
    /// Findings.
    pub findings: Findings,
    /// Compiler warnings.
    pub warnings: Vec<String>,
}

/// Run the pipeline offline.
///
/// # Errors
/// On load / compile / parse failures.
pub fn run(req: &ExplainRequest) -> anyhow::Result<Explanation> {
    let input = crate::load_input(&req.policy)?;
    let (snapshot, report) =
        compile(&input).map_err(|e| anyhow::anyhow!("resources do not compile:\n{}", e.0))?;
    let engine = Engine::new();
    let profile_classifier = routed_classify::from_profile(
        snapshot
            .core
            .profiles
            .get("default")
            .or_else(|| snapshot.core.profiles.values().next()),
    )
    .map_err(|e| {
        anyhow::anyhow!(
            "{e}; use findings.json or a heuristic/stub RouterProfile for offline explanations"
        )
    })?;
    let stub;
    let classifier: &dyn Classifier = match &req.findings {
        Some(f) => {
            stub = StubClassifier::returning(f.clone());
            &stub
        }
        None => profile_classifier.as_ref(),
    };
    let pipeline = Pipeline {
        engine: &engine,
        snapshot: &snapshot,
        classifier,
    };
    let ctx = DecisionContext { id: req.id.clone() };
    let mut out = if req.overrides.estimated_input_tokens.is_some()
        || req.overrides.estimated_output_tokens.is_some()
    {
        // Re-run with overridden token estimates.
        let headers = routed_security::extract_headers(req.headers.iter());
        let parsed =
            routed_router::parse_request(&req.path, &req.request).context("request.json")?;
        let default_output = snapshot
            .core
            .profiles
            .values()
            .next()
            .map_or(256, |p| p.default_output_tokens);
        let mut di = parsed.to_input(&req.path, &headers, default_output);
        if let Some(t) = req.overrides.estimated_input_tokens {
            di.estimated_input_tokens = t;
        }
        if let Some(t) = req.overrides.estimated_output_tokens {
            di.estimated_output_tokens = t;
        }
        let findings = match &req.findings {
            Some(f) => f.clone(),
            None => classifier
                .classify(&parsed.classify_input)
                .unwrap_or_else(|e| Findings {
                    degraded: vec![routed_router::degraded_name(classifier.name(), &e)],
                    ..Default::default()
                }),
        };
        let decision = engine.decide(&snapshot, &di, &findings, &ctx);
        routed_router::Outcome {
            decision,
            input: di,
            findings,
            headers,
        }
    } else {
        pipeline
            .run(&req.path, req.headers.iter(), &req.request, None, &ctx)
            .context("request.json")?
    };
    for ignored in &out.headers.ignored {
        out.decision.notes.push(format!("ignored header {ignored}"));
    }
    Ok(Explanation {
        decision: out.decision,
        input: out.input,
        findings: out.findings,
        warnings: report.warnings().map(ToString::to_string).collect(),
    })
}

/// Whether to colourise output.
#[must_use]
pub fn use_color() -> bool {
    std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

fn paint(color: bool, code: &str, s: &str) -> String {
    if color {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_owned()
    }
}

/// Human-readable trace.
#[must_use]
pub fn render_trace(e: &Explanation, color: bool) -> String {
    let d = &e.decision;
    let mut s = String::new();
    let outcome = match d.outcome {
        Outcome::Route => paint(color, "32;1", "ROUTE"),
        Outcome::PassThrough => paint(color, "33;1", "PASS_THROUGH"),
        Outcome::Block => paint(color, "31;1", "BLOCK"),
    };
    let _ = writeln!(
        s,
        "{outcome}  policy={}  model={} -> {}",
        d.policy.as_deref().unwrap_or("-"),
        d.requested_model,
        d.gateway_model.as_deref().unwrap_or("-")
    );
    let _ = writeln!(
        s,
        "  data class: {}   task: {}   complexity: {}   risk: {}",
        d.data_class.as_deref().unwrap_or("-"),
        d.classification.task.as_deref().unwrap_or("-"),
        d.classification
            .complexity
            .map_or("-".to_string(), |c| format!("{c:?}").to_lowercase()),
        d.classification
            .risk_score
            .map_or("-".to_string(), |r| format!("{r:.3}"))
    );
    let _ = writeln!(
        s,
        "  tokens in/out: {}/{}   tenant: {}   hints: {:?}",
        e.input.estimated_input_tokens,
        e.input.estimated_output_tokens,
        e.input.tenant.as_deref().unwrap_or("-"),
        e.input.hints
    );
    if let Some(r) = &d.reason {
        let _ = writeln!(s, "  reason: {r}");
    }
    for c in &d.candidates {
        let line = match (&c.eliminated_by, c.selected) {
            (Some(r), _) => paint(
                color,
                "31",
                &format!("  x {:<20} eliminated by {r}", c.tier),
            ),
            (None, true) => paint(
                color,
                "32",
                &format!(
                    "  * {:<20} selected  quality={:.3} cost=EUR {:.6} score={:.4}",
                    c.tier,
                    c.predicted_quality.unwrap_or(0.0),
                    c.estimated_cost_eur.unwrap_or(0.0),
                    c.score.unwrap_or(0.0)
                ),
            ),
            (None, false) => format!(
                "    {:<20} quality={:.3} cost=EUR {:.6} score={:.4}",
                c.tier,
                c.predicted_quality.unwrap_or(0.0),
                c.estimated_cost_eur.unwrap_or(0.0),
                c.score.unwrap_or(0.0)
            ),
        };
        let _ = writeln!(s, "{line}");
    }
    if let (Some(c), Some(sv)) = (d.estimated_cost_eur, d.estimated_savings_eur) {
        let _ = writeln!(
            s,
            "  estimated cost EUR {c:.6}, savings EUR {sv:.6} vs most expensive surviving candidate"
        );
    }
    for n in &d.notes {
        let _ = writeln!(s, "  {}", paint(color, "33", &format!("note: {n}")));
    }
    for w in &e.warnings {
        let _ = writeln!(s, "  {}", paint(color, "33", &format!("compiler {w}")));
    }
    let _ = writeln!(s, "  snapshot {}", d.snapshot_hash);
    s
}
