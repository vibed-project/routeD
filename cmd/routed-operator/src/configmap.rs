// SPDX-License-Identifier: Apache-2.0
//! Publishes the compiled snapshot into a `ConfigMap` (ADR-0014 fallback
//! distribution path): the Helm chart mounts it into the router pod as a
//! file, and the router's compiled-snapshot file source loads it directly,
//! with no recompilation on the router side.

use k8s_openapi::api::core::v1::ConfigMap;
use kube::Client;
use kube::api::{Api, Patch, PatchParams};
use routed_snapshot::Snapshot;

const FIELD_MANAGER: &str = "routed-operator";

/// Key the snapshot JSON is stored under inside the `ConfigMap`.
pub const DATA_KEY: &str = "snapshot.json";

/// Create or update `name` in `namespace` with the snapshot's canonical JSON.
pub async fn publish(
    client: &Client,
    namespace: &str,
    name: &str,
    snapshot: &Snapshot,
) -> kube::Result<()> {
    let json = serde_json::to_string_pretty(snapshot).unwrap_or_else(|_| "{}".to_owned());
    // Server-side apply requires apiVersion and kind in the payload, which
    // the typed ConfigMap struct does not serialize; build the object as JSON.
    let cm = serde_json::json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": { "name": name, "namespace": namespace },
        "data": { DATA_KEY: json },
    });
    let api: Api<ConfigMap> = Api::namespaced(client.clone(), namespace);
    let pp = PatchParams::apply(FIELD_MANAGER).force();
    api.patch(name, &pp, &Patch::Apply(&cm)).await?;
    Ok(())
}
