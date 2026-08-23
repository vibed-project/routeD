// SPDX-License-Identifier: Apache-2.0
//! PII detection, injection / risk signals, and the restriction-only header
//! hint model.
//!
//! Classifies only; never redacts. Untrusted headers can only make a decision
//! more restrictive (ADR-0007): this crate parses them into
//! [`routed_decision::RequestHints`], a type that cannot express relaxation,
//! and tells ingress layers which inbound headers to strip.

pub mod headers;
pub mod injection;
pub mod pii;

pub use headers::{RequestHeaders, extract_headers, is_routed_header};
pub use injection::{InjectionSignal, score_injection};
pub use pii::{PiiMatch, detect_pii};
