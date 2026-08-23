// SPDX-License-Identifier: Apache-2.0
//! Watches the four CRDs and recompiles a snapshot on every change.
//!
//! One global compile, not four independent ones (ADR-0014): whenever any of
//! `ModelTier`, `DataClass`, `RoutingPolicy` or `RouterProfile` changes, every
//! kind is relisted and `routed_policy::compile` runs once over the full set.

use std::pin::Pin;

use futures_util::stream::{self, Stream, StreamExt};
use kube::api::{Api, ListParams};
use kube::runtime::WatchStreamExt as _;
use kube::runtime::watcher::{Config, Event, watcher};
use kube::{Client, ResourceExt};
use routed_api::v1alpha1::{DataClass, ModelTier, RouterProfile, RoutingPolicy};
use routed_policy::{CompileError, CompileInput, CompileReport};
use routed_snapshot::Snapshot;

/// Objects behind the most recent compile attempt, alongside its outcome.
/// Kept together so status writers don't need a second round trip to the API
/// server to know what to annotate.
pub struct Compiled {
    /// The new snapshot, or `None` if compilation failed (the previous
    /// snapshot, if any, keeps serving).
    pub snapshot: Option<Snapshot>,
    /// Diagnostics keyed by `(kind, "namespace/name")` inside each `Diag`.
    pub report: CompileReport,
    /// Objects considered, for status writes.
    pub tiers: Vec<ModelTier>,
    /// Objects considered, for status writes.
    pub data_classes: Vec<DataClass>,
    /// Objects considered, for status writes.
    pub policies: Vec<RoutingPolicy>,
    /// Objects considered, for status writes.
    pub profiles: Vec<RouterProfile>,
}

/// A change worth recompiling for: the initial sync completed, or a steady
/// state add/update/delete happened. `Event::Init` / `Event::InitApply` are
/// intentionally dropped so an initial list of N objects triggers one
/// recompile, not N.
fn triggers<K>(api: Api<K>) -> Pin<Box<dyn Stream<Item = ()> + Send>>
where
    K: kube::Resource + Clone + std::fmt::Debug + Send + serde::de::DeserializeOwned + 'static,
{
    watcher(api, Config::default())
        .default_backoff()
        .filter_map(|ev| async move {
            match ev {
                Ok(Event::InitDone | Event::Apply(_) | Event::Delete(_)) => Some(()),
                Ok(Event::Init | Event::InitApply(_)) => None,
                Err(e) => {
                    tracing::warn!(error = %e, "watch stream error");
                    None
                }
            }
        })
        .boxed()
}

/// Merge the four kinds' watch streams into a single recompile trigger.
pub fn trigger_stream(client: &Client, namespace: Option<&str>) -> impl Stream<Item = ()> {
    let (t, dc, rp, prof): (
        Api<ModelTier>,
        Api<DataClass>,
        Api<RoutingPolicy>,
        Api<RouterProfile>,
    ) = namespace.map_or_else(
        || {
            (
                Api::all(client.clone()),
                Api::all(client.clone()),
                Api::all(client.clone()),
                Api::all(client.clone()),
            )
        },
        |ns| {
            (
                Api::namespaced(client.clone(), ns),
                Api::namespaced(client.clone(), ns),
                Api::namespaced(client.clone(), ns),
                Api::namespaced(client.clone(), ns),
            )
        },
    );
    stream::select_all([triggers(t), triggers(dc), triggers(rp), triggers(prof)])
}

/// List every kind into a compiler input (shared by the reconcile loop and
/// the admission webhook).
pub async fn list_input(client: &Client, namespace: Option<&str>) -> kube::Result<CompileInput> {
    let lp = ListParams::default();
    let (tiers, data_classes, policies, profiles) = if let Some(ns) = namespace {
        let t: Api<ModelTier> = Api::namespaced(client.clone(), ns);
        let dc: Api<DataClass> = Api::namespaced(client.clone(), ns);
        let rp: Api<RoutingPolicy> = Api::namespaced(client.clone(), ns);
        let prof: Api<RouterProfile> = Api::namespaced(client.clone(), ns);
        (
            t.list(&lp).await?.items,
            dc.list(&lp).await?.items,
            rp.list(&lp).await?.items,
            prof.list(&lp).await?.items,
        )
    } else {
        let t: Api<ModelTier> = Api::all(client.clone());
        let dc: Api<DataClass> = Api::all(client.clone());
        let rp: Api<RoutingPolicy> = Api::all(client.clone());
        let prof: Api<RouterProfile> = Api::all(client.clone());
        (
            t.list(&lp).await?.items,
            dc.list(&lp).await?.items,
            rp.list(&lp).await?.items,
            prof.list(&lp).await?.items,
        )
    };
    tracing::debug!(
        tiers = tiers.len(),
        data_classes = data_classes.len(),
        policies = policies.len(),
        profiles = profiles.len(),
        "listed CRDs"
    );
    Ok(CompileInput {
        tiers,
        data_classes,
        policies,
        profiles,
    })
}

/// List every kind and run the compiler once over the current cluster state.
pub async fn compile_once(client: &Client, namespace: Option<&str>) -> kube::Result<Compiled> {
    let input = list_input(client, namespace).await?;
    let (snapshot, report) = match routed_policy::compile(&input) {
        Ok((snapshot, report)) => (Some(snapshot), report),
        Err(CompileError(report)) => (None, report),
    };
    for d in &report.diags {
        match d.level {
            routed_policy::Level::Error => {
                tracing::warn!(kind = %d.kind, name = %d.name, field = %d.field, "compile error: {}", d.message);
            }
            routed_policy::Level::Warning => {
                tracing::debug!(kind = %d.kind, name = %d.name, field = %d.field, "compile warning: {}", d.message);
            }
        }
    }
    Ok(Compiled {
        snapshot,
        report,
        tiers: input.tiers,
        data_classes: input.data_classes,
        policies: input.policies,
        profiles: input.profiles,
    })
}

/// `namespace/name`, matching `routed_policy`'s diagnostic naming.
pub fn ns_name(r: &impl ResourceExt) -> String {
    format!(
        "{}/{}",
        r.namespace().unwrap_or_else(|| "default".into()),
        r.name_any()
    )
}
