// SPDX-License-Identifier: Apache-2.0
//! The `routed.io/v1alpha1` API version.

mod common;
mod dataclass;
mod modeltier;
mod routerprofile;
mod routingpolicy;

pub use common::*;
pub use dataclass::*;
pub use modeltier::*;
pub use routerprofile::*;
pub use routingpolicy::*;

use kube::CustomResourceExt;

/// API group owning all routeD custom resources.
pub const GROUP: &str = "routed.io";
/// API version.
pub const VERSION: &str = "v1alpha1";
/// Kinds defined in this group/version.
pub const KINDS: [&str; 4] = ["ModelTier", "DataClass", "RoutingPolicy", "RouterProfile"];

/// `group/version` string as used in `apiVersion`.
#[must_use]
pub fn api_version() -> String {
    format!("{GROUP}/{VERSION}")
}

/// All CRD manifests of this API version, in a stable order.
#[must_use]
pub fn crds()
-> Vec<k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition>
{
    vec![
        ModelTier::crd(),
        DataClass::crd(),
        RoutingPolicy::crd(),
        RouterProfile::crd(),
    ]
}
