// SPDX-License-Identifier: Apache-2.0
//! Parse multi-document YAML / JSON into typed routeD resources.

use routed_api::v1alpha1::{DataClass, ModelTier, RouterProfile, RoutingPolicy, api_version};
use serde::Deserialize;

use crate::CompileInput;

/// Any routeD resource.
#[derive(Clone, Debug)]
pub enum Resource {
    /// A `ModelTier`.
    ModelTier(Box<ModelTier>),
    /// A `DataClass`.
    DataClass(Box<DataClass>),
    /// A `RoutingPolicy`.
    RoutingPolicy(Box<RoutingPolicy>),
    /// A `RouterProfile`.
    RouterProfile(Box<RouterProfile>),
}

/// Parse error.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// YAML syntax or schema error.
    #[error("document {index}: {source}")]
    Yaml {
        /// Zero-based document index within the input.
        index: usize,
        /// Underlying error.
        #[source]
        source: serde_yaml_ng::Error,
    },
    /// Unknown `apiVersion` / `kind`.
    #[error("document {index}: unsupported {api_version} {kind}")]
    Unsupported {
        /// Zero-based document index within the input.
        index: usize,
        /// `apiVersion` seen.
        api_version: String,
        /// `kind` seen.
        kind: String,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Head {
    api_version: String,
    kind: String,
}

/// Parse all documents in a YAML stream (a single JSON document also works).
///
/// Empty documents are skipped. `List` kinds are not supported.
///
/// # Errors
/// On the first malformed or unsupported document.
pub fn parse_documents(text: &str) -> Result<Vec<Resource>, LoadError> {
    let mut out = Vec::new();
    for (index, de) in serde_yaml_ng::Deserializer::from_str(text).enumerate() {
        let value = serde_yaml_ng::Value::deserialize(de)
            .map_err(|source| LoadError::Yaml { index, source })?;
        if value.is_null() {
            continue;
        }
        let head: Head = serde_yaml_ng::from_value(value.clone())
            .map_err(|source| LoadError::Yaml { index, source })?;
        if head.api_version != api_version() {
            return Err(LoadError::Unsupported {
                index,
                api_version: head.api_version,
                kind: head.kind,
            });
        }
        let parse = |source| LoadError::Yaml { index, source };
        let res = match head.kind.as_str() {
            "ModelTier" => {
                Resource::ModelTier(Box::new(serde_yaml_ng::from_value(value).map_err(parse)?))
            }
            "DataClass" => {
                Resource::DataClass(Box::new(serde_yaml_ng::from_value(value).map_err(parse)?))
            }
            "RoutingPolicy" => {
                Resource::RoutingPolicy(Box::new(serde_yaml_ng::from_value(value).map_err(parse)?))
            }
            "RouterProfile" => {
                Resource::RouterProfile(Box::new(serde_yaml_ng::from_value(value).map_err(parse)?))
            }
            _ => {
                return Err(LoadError::Unsupported {
                    index,
                    api_version: head.api_version,
                    kind: head.kind,
                });
            }
        };
        out.push(res);
    }
    Ok(out)
}

/// Group parsed resources into a [`CompileInput`].
#[must_use]
pub fn into_input(resources: Vec<Resource>) -> CompileInput {
    let mut input = CompileInput::default();
    for r in resources {
        match r {
            Resource::ModelTier(t) => input.tiers.push(*t),
            Resource::DataClass(d) => input.data_classes.push(*d),
            Resource::RoutingPolicy(p) => input.policies.push(*p),
            Resource::RouterProfile(p) => input.profiles.push(*p),
        }
    }
    input
}
