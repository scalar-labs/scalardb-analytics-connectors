//! Validation of Substrait extension declarations and anchors.

use std::collections::HashMap;

use substrait::proto::extensions::{self, simple_extension_declaration};

use super::{LOGICAL_TYPE_NAME, LOGICAL_URI};
use crate::RequestError;

/// Validated lookup tables for extension anchors referenced by plan payloads.
///
/// Type and function declarations are validated and indexed once. Schema
/// conversion resolves user-defined types, while expression validation and
/// SQL translation reuse the function index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ExtensionRegistry {
    pub(super) types: HashMap<u32, String>,
    pub(super) functions: HashMap<u32, FunctionExtension>,
}

/// One function declaration after its URI anchor has been resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FunctionExtension {
    pub(super) uri: String,
    pub(super) name: String,
}

impl ExtensionRegistry {
    /// Validates extension declarations and indexes declared type and function anchors.
    pub(super) fn new(
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
                                FunctionExtension {
                                    uri: uri.to_owned(),
                                    name: function.name.clone(),
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
                    if data_type.name == LOGICAL_TYPE_NAME && uri != LOGICAL_URI {
                        return Err(RequestError::undeclared(
                            "the logical type must use the ScalarDB Analytics extension URI",
                        ));
                    }
                    if data_type.name.is_empty()
                        || types
                            .insert(data_type.type_anchor, data_type.name.clone())
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
}

/// Resolves an extension URI reference and rejects undeclared anchors.
fn require_uri<'a>(uris: &HashMap<u32, &'a str>, reference: u32) -> Result<&'a str, RequestError> {
    uris.get(&reference).copied().ok_or_else(|| {
        RequestError::invalid(format!(
            "extension URI reference {reference} is not declared"
        ))
    })
}
