// SPDX-License-Identifier: Apache-2.0
//! Writes `status.conditions` (and, on `RoutingPolicy`, `compiledHash`) from a
//! compile's diagnostics back onto every object (ADR-0014).
//!
//! Writes are skipped when the stored status already matches, and
//! `lastTransitionTime` is preserved while the condition value is unchanged.
//! Both matter beyond etiquette: a status write bumps the object's
//! `resourceVersion`, which fires our own CRD watcher and triggers another
//! reconcile; unconditional writes with a fresh timestamp would loop forever.

use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use kube::api::{Api, Patch, PatchParams};
use kube::{Client, Resource as _, ResourceExt};
use routed_policy::{CompileReport, Level};
use serde_json::json;

use crate::reconcile::{Compiled, ns_name};

const FIELD_MANAGER: &str = "routed-operator";
const READY: &str = "Ready";

/// What the Ready condition should say for one object.
struct Desired<'a> {
    error: Option<&'a str>,
}

impl Desired<'_> {
    fn status(&self) -> &'static str {
        if self.error.is_none() {
            "True"
        } else {
            "False"
        }
    }
    fn reason(&self) -> &'static str {
        if self.error.is_none() {
            "Compiled"
        } else {
            "CompileError"
        }
    }
    fn message(&self) -> &str {
        self.error.unwrap_or("compiled into the current snapshot")
    }
}

fn find_ready(conds: &[Condition]) -> Option<&Condition> {
    conds.iter().find(|c| c.type_ == READY)
}

/// Whether the stored status already says what we would write.
fn up_to_date(
    desired: &Desired<'_>,
    conds: &[Condition],
    observed: Option<i64>,
    generation: Option<i64>,
) -> bool {
    observed == generation
        && find_ready(conds).is_some_and(|c| {
            c.status == desired.status()
                && c.reason == desired.reason()
                && c.message == desired.message()
        })
}

/// The condition to write: `lastTransitionTime` is carried over from the
/// stored condition while its status value is unchanged.
fn ready_condition(desired: &Desired<'_>, existing: &[Condition]) -> Condition {
    let carried = find_ready(existing)
        .filter(|c| c.status == desired.status())
        .map(|c| c.last_transition_time.clone());
    Condition {
        type_: READY.to_owned(),
        status: desired.status().to_owned(),
        reason: desired.reason().to_owned(),
        message: desired.message().to_owned(),
        observed_generation: None,
        last_transition_time: carried.unwrap_or_else(|| Time(k8s_openapi::jiff::Timestamp::now())),
    }
}

/// First error message diagnosed against `(kind, "namespace/name")`, if any.
fn first_error<'a>(report: &'a CompileReport, kind: &str, name: &str) -> Option<&'a str> {
    report
        .diags
        .iter()
        .find(|d| d.level == Level::Error && d.kind == kind && d.name == name)
        .map(|d| d.message.as_str())
}

async fn patch_one<K>(client: &Client, obj: &K, cond: Condition, extra: serde_json::Value)
where
    K: kube::Resource<Scope = k8s_openapi::NamespaceResourceScope>
        + Clone
        + std::fmt::Debug
        + serde::de::DeserializeOwned
        + serde::Serialize,
    K::DynamicType: Default,
{
    let ns = obj.namespace().unwrap_or_else(|| "default".to_owned());
    let api: Api<K> = Api::namespaced(client.clone(), &ns);
    let mut status = json!({
        "conditions": [cond],
        "observedGeneration": obj.meta().generation,
    });
    if let (Some(status_obj), Some(extra_obj)) = (status.as_object_mut(), extra.as_object()) {
        for (k, v) in extra_obj {
            status_obj.insert(k.clone(), v.clone());
        }
    }
    let patch = Patch::Merge(json!({ "status": status }));
    // Merge, not server-side apply: force() is Apply-only, and a merge patch
    // needs no apiVersion/kind envelope. field_manager is optional for merge.
    let pp = PatchParams {
        field_manager: Some(FIELD_MANAGER.to_owned()),
        ..PatchParams::default()
    };
    if let Err(e) = api.patch_status(&obj.name_any(), &pp, &patch).await {
        tracing::warn!(name = %ns_name(obj), error = %e, "failed to patch status");
    }
}

/// Write status onto every object considered in `compiled`, skipping objects
/// whose stored status is already correct.
pub async fn apply(client: &Client, compiled: &Compiled) {
    let snapshot_hash = compiled.snapshot.as_ref().map(|s| s.hash.clone());

    for o in &compiled.tiers {
        let desired = Desired {
            error: first_error(&compiled.report, "ModelTier", &ns_name(o)),
        };
        let (conds, observed) = o.status.as_ref().map_or((&[] as &[Condition], None), |s| {
            (s.conditions.as_slice(), s.observed_generation)
        });
        if up_to_date(&desired, conds, observed, o.meta().generation) {
            continue;
        }
        patch_one(client, o, ready_condition(&desired, conds), json!({})).await;
    }

    for o in &compiled.data_classes {
        let desired = Desired {
            error: first_error(&compiled.report, "DataClass", &ns_name(o)),
        };
        let (conds, observed) = o.status.as_ref().map_or((&[] as &[Condition], None), |s| {
            (s.conditions.as_slice(), s.observed_generation)
        });
        if up_to_date(&desired, conds, observed, o.meta().generation) {
            continue;
        }
        patch_one(client, o, ready_condition(&desired, conds), json!({})).await;
    }

    for o in &compiled.profiles {
        let desired = Desired {
            error: first_error(&compiled.report, "RouterProfile", &ns_name(o)),
        };
        let (conds, observed) = o.status.as_ref().map_or((&[] as &[Condition], None), |s| {
            (s.conditions.as_slice(), s.observed_generation)
        });
        if up_to_date(&desired, conds, observed, o.meta().generation) {
            continue;
        }
        patch_one(client, o, ready_condition(&desired, conds), json!({})).await;
    }

    for o in &compiled.policies {
        let desired = Desired {
            error: first_error(&compiled.report, "RoutingPolicy", &ns_name(o)),
        };
        let (conds, observed, stored_hash) =
            o.status
                .as_ref()
                .map_or((&[] as &[Condition], None, None), |s| {
                    (
                        s.conditions.as_slice(),
                        s.observed_generation,
                        s.compiled_hash.as_deref(),
                    )
                });
        let hash_current = desired.error.is_some() || stored_hash == snapshot_hash.as_deref();
        if hash_current && up_to_date(&desired, conds, observed, o.meta().generation) {
            continue;
        }
        let extra = if desired.error.is_none() {
            json!({ "compiledHash": snapshot_hash })
        } else {
            json!({})
        };
        patch_one(client, o, ready_condition(&desired, conds), extra).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use routed_policy::{CompileReport, Diag, Level};

    fn report_with(kind: &str, name: &str, level: Level, message: &str) -> CompileReport {
        CompileReport {
            diags: vec![Diag {
                level,
                kind: kind.to_owned(),
                name: name.to_owned(),
                field: "spec".to_owned(),
                message: message.to_owned(),
            }],
        }
    }

    fn cond(status: &str, reason: &str, message: &str) -> Condition {
        Condition {
            type_: READY.to_owned(),
            status: status.to_owned(),
            reason: reason.to_owned(),
            message: message.to_owned(),
            observed_generation: None,
            last_transition_time: Time(k8s_openapi::jiff::Timestamp::UNIX_EPOCH),
        }
    }

    #[test]
    fn desired_shapes() {
        let ok = Desired { error: None };
        assert_eq!(ok.status(), "True");
        assert_eq!(ok.reason(), "Compiled");
        let bad = Desired {
            error: Some("boom"),
        };
        assert_eq!(bad.status(), "False");
        assert_eq!(bad.reason(), "CompileError");
        assert_eq!(bad.message(), "boom");
    }

    #[test]
    fn first_error_matches_kind_and_name() {
        let report = report_with("ModelTier", "ai-platform/t1", Level::Error, "bad price");
        assert_eq!(
            first_error(&report, "ModelTier", "ai-platform/t1"),
            Some("bad price")
        );
        // A different kind with the same name/namespace does not match.
        assert_eq!(first_error(&report, "DataClass", "ai-platform/t1"), None);
        // Warnings never count as errors.
        let warn = report_with("ModelTier", "ai-platform/t1", Level::Warning, "heads up");
        assert_eq!(first_error(&warn, "ModelTier", "ai-platform/t1"), None);
    }

    #[test]
    fn unchanged_status_is_skipped() {
        let desired = Desired { error: None };
        let stored = [cond(
            "True",
            "Compiled",
            "compiled into the current snapshot",
        )];
        assert!(up_to_date(&desired, &stored, Some(3), Some(3)));
        // A new generation forces a write even with an identical condition.
        assert!(!up_to_date(&desired, &stored, Some(2), Some(3)));
        // A different outcome forces a write.
        let failing = Desired {
            error: Some("boom"),
        };
        assert!(!up_to_date(&failing, &stored, Some(3), Some(3)));
        // No stored condition at all forces a write.
        assert!(!up_to_date(&desired, &[], Some(3), Some(3)));
    }

    #[test]
    fn transition_time_preserved_while_status_unchanged() {
        let desired = Desired { error: None };
        let stored = [cond("True", "Compiled", "old message")];
        let written = ready_condition(&desired, &stored);
        assert_eq!(
            written.last_transition_time.0,
            k8s_openapi::jiff::Timestamp::UNIX_EPOCH,
            "same status value keeps the stored transition time"
        );
        // A True -> False transition stamps a new time.
        let failing = Desired {
            error: Some("boom"),
        };
        let written = ready_condition(&failing, &stored);
        assert_ne!(
            written.last_transition_time.0,
            k8s_openapi::jiff::Timestamp::UNIX_EPOCH
        );
    }
}
