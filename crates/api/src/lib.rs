// SPDX-License-Identifier: Apache-2.0
//! CRD types for the `routed.io/v1alpha1` API group.
//!
//! Pure data types plus the `kube` derive macros (no Kubernetes client, no
//! tokio; enforced by `scripts/check-crate-boundary.sh`). The CRD manifests
//! under `config/crd/` are generated from these types by `routedctl crd gen`.

pub mod v1alpha1;

#[cfg(test)]
mod tests {
    use super::v1alpha1;

    #[test]
    fn api_version_string() {
        assert_eq!(v1alpha1::api_version(), "routed.io/v1alpha1");
        assert_eq!(v1alpha1::KINDS.len(), 4);
    }
}
