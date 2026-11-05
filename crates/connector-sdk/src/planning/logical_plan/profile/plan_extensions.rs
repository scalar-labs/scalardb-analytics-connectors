//! Resolution of one plan's extension anchors against the connector profile.

use std::collections::HashMap;

use substrait::proto::extensions::{self, simple_extension_declaration};

use super::{CONNECTOR_PROFILE, FunctionDeclaration, ProfileFunction, ProfileType};
use crate::RequestError;

/// Plan-specific extension anchors resolved against the connector profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::planning::logical_plan) struct PlanExtensions {
    types: HashMap<u32, ResolvedType>,
    functions: HashMap<u32, ResolvedFunction>,
}

/// One plan declaration paired with its resolved URI.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ResolvedDeclaration {
    uri: String,
    name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResolvedFunction {
    declaration: ResolvedDeclaration,
    function: Option<ProfileFunction>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResolvedType {
    declaration: ResolvedDeclaration,
}

impl PlanExtensions {
    /// Validates extension declarations and indexes declared type and function anchors.
    pub(in crate::planning::logical_plan) fn new(
        uris: &[extensions::SimpleExtensionUri],
        declarations: &[extensions::SimpleExtensionDeclaration],
    ) -> Result<Self, RequestError> {
        let mut uri_anchors = HashMap::new();
        for uri in uris {
            if uri.uri.is_empty()
                || uri_anchors
                    .insert(uri.extension_uri_anchor, uri.uri.as_str())
                    .is_some()
            {
                return Err(RequestError::invalid(
                    "extension URI anchors must be unique and non-empty",
                ));
            }
        }
        let mut types = HashMap::new();
        let mut functions = HashMap::new();
        for declaration in declarations {
            match declaration.mapping_type.as_ref() {
                Some(simple_extension_declaration::MappingType::ExtensionFunction(function)) => {
                    let uri = require_uri(&uri_anchors, function.extension_uri_reference)?;
                    if function.name.is_empty()
                        || functions
                            .insert(
                                function.function_anchor,
                                ResolvedFunction {
                                    declaration: ResolvedDeclaration {
                                        uri: uri.to_owned(),
                                        name: function.name.clone(),
                                    },
                                    function: CONNECTOR_PROFILE.function(uri, &function.name),
                                },
                            )
                            .is_some()
                    {
                        return Err(RequestError::invalid(
                            "function anchors must be unique and named",
                        ));
                    }
                }
                Some(simple_extension_declaration::MappingType::ExtensionType(data_type)) => {
                    let uri = require_uri(&uri_anchors, data_type.extension_uri_reference)?;
                    let profile_type = CONNECTOR_PROFILE.data_type(uri, &data_type.name);
                    if profile_type.is_none() && CONNECTOR_PROFILE.has_type_name(&data_type.name) {
                        return Err(RequestError::undeclared(
                            "the extension type must use the URI declared by the connector profile",
                        ));
                    }
                    if data_type.name.is_empty()
                        || types
                            .insert(
                                data_type.type_anchor,
                                ResolvedType {
                                    declaration: ResolvedDeclaration {
                                        uri: uri.to_owned(),
                                        name: data_type.name.clone(),
                                    },
                                },
                            )
                            .is_some()
                    {
                        return Err(RequestError::invalid(
                            "type anchors must be unique and named",
                        ));
                    }
                }
                Some(simple_extension_declaration::MappingType::ExtensionTypeVariation(_)) => {
                    return Err(RequestError::undeclared(
                        "extension type variations are not accepted",
                    ));
                }
                None => return Err(RequestError::invalid("extension declaration is empty")),
            }
        }
        Ok(Self { types, functions })
    }

    /// Resolves one declared scalar-function anchor to its URI and signature.
    pub(in crate::planning::logical_plan) fn function_declaration(
        &self,
        anchor: u32,
    ) -> Option<FunctionDeclaration<'_>> {
        let function = self.functions.get(&anchor)?;
        Some(FunctionDeclaration {
            uri: &function.declaration.uri,
            name: &function.declaration.name,
        })
    }

    /// Resolves and classifies one declared scalar function.
    pub(in crate::planning::logical_plan) fn profile_function(
        &self,
        anchor: u32,
    ) -> Option<ProfileFunction> {
        self.functions
            .get(&anchor)
            .and_then(|function| function.function)
    }

    /// Finds the plan-local anchor for a function recognized by the profile.
    pub(in crate::planning::logical_plan) fn profile_function_anchor(
        &self,
        profile_function: ProfileFunction,
    ) -> u32 {
        self.functions
            .iter()
            .find_map(|(anchor, function)| {
                (function.function == Some(profile_function)).then_some(*anchor)
            })
            .expect("logical operators retain functions resolved from this plan")
    }

    /// Finds the plan-local anchor for an otherwise unsupported declaration.
    pub(in crate::planning::logical_plan) fn declared_function_anchor(
        &self,
        uri: &str,
        name: &str,
    ) -> u32 {
        self.functions
            .iter()
            .find_map(|(anchor, function)| {
                (function.declaration.uri == uri && function.declaration.name == name)
                    .then_some(*anchor)
            })
            .expect("logical expressions retain declarations from this plan")
    }

    /// Returns the plan-local anchor for the connector logical type declaration.
    pub(in crate::planning::logical_plan) fn logical_type_anchor(&self) -> u32 {
        self.types
            .iter()
            .find_map(|(anchor, data_type)| {
                (CONNECTOR_PROFILE
                    .data_type(&data_type.declaration.uri, &data_type.declaration.name)
                    == Some(ProfileType::Logical))
                .then_some(*anchor)
            })
            .expect("a logical Analytics type requires its validated declaration")
    }

    /// Returns whether an anchor declares the connector profile's logical type.
    pub(in crate::planning::logical_plan) fn is_logical_type(&self, anchor: u32) -> bool {
        self.types.get(&anchor).is_some_and(|data_type| {
            CONNECTOR_PROFILE.data_type(&data_type.declaration.uri, &data_type.declaration.name)
                == Some(ProfileType::Logical)
        })
    }
}

/// Resolves an extension URI reference and rejects undeclared anchors.
fn require_uri<'a>(uris: &HashMap<u32, &'a str>, reference: u32) -> Result<&'a str, RequestError> {
    uris.get(&reference).copied().ok_or_else(|| {
        RequestError::invalid(format!(
            "extension URI reference {reference} is not declared"
        ))
    })
}
