// SPDX-License-Identifier: Apache-2.0
//! Pluggable caller authentication for the decision APIs (ADR-0020).
//!
//! The router itself is credential-agnostic: the default [`AllowAll`]
//! authenticator preserves the historical behaviour (every caller is
//! anonymous and allowed). Deployments that need to authenticate callers of
//! `/v1/decide` and `/v1/feedback` plug an implementation in via
//! [`crate::AppState::with_authenticator`]. An authenticator can only deny a
//! request or attach an identity — it cannot alter the decision itself.

use axum::http::HeaderMap;

/// The authenticated caller, as established by an [`Authenticator`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Identity {
    /// Stable subject identifier (empty for anonymous).
    pub subject: String,
    /// Group / role memberships, verbatim from the credential source.
    pub groups: Vec<String>,
}

impl Identity {
    /// The anonymous identity used by [`AllowAll`].
    #[must_use]
    pub fn anonymous() -> Self {
        Self::default()
    }
}

/// Outcome of authenticating one request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthDecision {
    /// The request proceeds with this identity.
    Allow(Identity),
    /// The request is rejected before any classification or decision work.
    Deny {
        /// HTTP status to return (401 or 403).
        status: u16,
        /// Short machine-readable reason; never echoes credential material.
        reason: String,
    },
}

/// Authenticates callers of the decision APIs from request headers.
///
/// Implementations must be cheap per call (cache upstream key material) and
/// must never log or propagate credential values.
pub trait Authenticator: Send + Sync {
    /// Authenticate one request from its headers.
    fn authenticate(&self, headers: &HeaderMap) -> AuthDecision;
    /// Implementation name for logs and diagnostics.
    fn name(&self) -> &'static str;
}

/// Default authenticator: every caller is anonymous and allowed.
pub struct AllowAll;

impl Authenticator for AllowAll {
    fn authenticate(&self, _headers: &HeaderMap) -> AuthDecision {
        AuthDecision::Allow(Identity::anonymous())
    }
    fn name(&self) -> &'static str {
        "allow-all"
    }
}
