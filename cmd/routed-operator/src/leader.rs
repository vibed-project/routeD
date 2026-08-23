// SPDX-License-Identifier: Apache-2.0
//! `coordination.k8s.io/v1 Lease`-based leader election (ADR-0014). Gates
//! only the write paths (status conditions, fallback `ConfigMap`); every
//! replica keeps compiling and serving gRPC watchers regardless of who
//! holds the lease.
//!
//! Deliberately not strictly mutually exclusive: writes it gates are
//! idempotent, so a short overlap during a handover causes at most a
//! duplicate write, never corruption. This keeps the implementation to a
//! plain get-then-create-or-patch instead of a compare-and-swap protocol.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use k8s_openapi::api::coordination::v1::{Lease, LeaseSpec};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{MicroTime, ObjectMeta};
use k8s_openapi::jiff::Timestamp;
use kube::Client;
use kube::api::{Api, Patch, PatchParams, PostParams};

const FIELD_MANAGER: &str = "routed-operator";
const LEASE_DURATION_SECS: i32 = 15;

/// Shared leader flag; cheap to clone and check from any task.
#[derive(Clone)]
pub struct Leadership {
    leading: Arc<AtomicBool>,
}

impl Leadership {
    /// Always report as leader (used when `--leader-elect` is off).
    pub fn always() -> Self {
        Self {
            leading: Arc::new(AtomicBool::new(true)),
        }
    }

    /// Current leadership state.
    pub fn is_leader(&self) -> bool {
        self.leading.load(Ordering::Relaxed)
    }
}

/// Renew or attempt to acquire the lease every third of its duration; runs
/// until the process exits.
pub fn spawn(client: Client, namespace: String, name: String, identity: String) -> Leadership {
    let leading = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&leading);
    tokio::spawn(async move {
        loop {
            let result = tick(&client, &namespace, &name, &identity).await;
            match result {
                Ok(is_leader) => flag.store(is_leader, Ordering::Relaxed),
                Err(e) => {
                    tracing::warn!(error = %e, "leader election tick failed");
                    flag.store(false, Ordering::Relaxed);
                }
            }
            tokio::time::sleep(Duration::from_secs(
                u64::try_from(LEASE_DURATION_SECS / 3).unwrap_or(5),
            ))
            .await;
        }
    });
    Leadership { leading }
}

async fn tick(client: &Client, namespace: &str, name: &str, identity: &str) -> kube::Result<bool> {
    let api: Api<Lease> = Api::namespaced(client.clone(), namespace);
    let now = MicroTime(Timestamp::now());

    let Some(existing) = api.get_opt(name).await? else {
        let lease = Lease {
            metadata: ObjectMeta {
                name: Some(name.to_owned()),
                namespace: Some(namespace.to_owned()),
                ..Default::default()
            },
            spec: Some(LeaseSpec {
                holder_identity: Some(identity.to_owned()),
                lease_duration_seconds: Some(LEASE_DURATION_SECS),
                acquire_time: Some(now.clone()),
                renew_time: Some(now),
                lease_transitions: Some(0),
                ..Default::default()
            }),
        };
        return Ok(api.create(&PostParams::default(), &lease).await.is_ok());
    };

    let spec = existing.spec.unwrap_or_default();
    let is_me = spec.holder_identity.as_deref() == Some(identity);
    let expired = spec.renew_time.as_ref().is_none_or(|rt| {
        let dur = i64::from(spec.lease_duration_seconds.unwrap_or(LEASE_DURATION_SECS));
        now.0.as_second() - rt.0.as_second() > dur
    });
    if !is_me && !expired {
        return Ok(false);
    }

    let transitions = spec.lease_transitions.unwrap_or(0) + i32::from(!is_me);
    let acquire_time = if is_me {
        spec.acquire_time.unwrap_or_else(|| now.clone())
    } else {
        now.clone()
    };
    // Server-side apply requires apiVersion and kind in the payload, which
    // the typed Lease struct does not serialize; build the object as JSON.
    let patch = serde_json::json!({
        "apiVersion": "coordination.k8s.io/v1",
        "kind": "Lease",
        "metadata": { "name": name, "namespace": namespace },
        "spec": {
            "holderIdentity": identity,
            "leaseDurationSeconds": LEASE_DURATION_SECS,
            "acquireTime": acquire_time,
            "renewTime": now,
            "leaseTransitions": transitions,
        },
    });
    let pp = PatchParams::apply(FIELD_MANAGER).force();
    api.patch(name, &pp, &Patch::Apply(&patch)).await?;
    Ok(true)
}
