//! Validation of Substrait extension declarations and anchors.

use std::collections::HashMap;

use substrait::proto::extensions::{self, simple_extension_declaration};

use super::{BOOLEAN_URI, COMPARISON_URI, LOGICAL_TYPE_NAME, LOGICAL_URI};
use crate::RequestError;

pub(super) struct ExtensionRegistry {
    pub(super) types: HashMap<u32, String>,
}

impl ExtensionRegistry {
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
        let mut function_anchors = HashMap::new();
        let mut types = HashMap::new();
        for declaration in declarations {
            match declaration.mapping_type.as_ref() {
                Some(simple_extension_declaration::MappingType::ExtensionFunction(function)) => {
                    let uri = require_uri(&uri_anchors, function.extension_uri_reference)?;
                    let base_name = function.name.split(':').next().unwrap_or_default();
                    if let Some(expected_uri) = expected_function_uri(base_name) {
                        if uri != expected_uri {
                            return Err(RequestError::undeclared(format!(
                                "function {base_name:?} must use its pinned standard extension URI"
                            )));
                        }
                    }
                    if function.name.is_empty()
                        || function_anchors
                            .insert(function.function_anchor, ())
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
        Ok(Self { types })
    }
}

fn expected_function_uri(name: &str) -> Option<&'static str> {
    match name {
        "equal" | "not_equal" | "lt" | "lte" | "gt" | "gte" => Some(COMPARISON_URI),
        "and" | "or" | "is_null" | "is_not_null" => Some(BOOLEAN_URI),
        _ => None,
    }
}

fn require_uri<'a>(uris: &HashMap<u32, &'a str>, reference: u32) -> Result<&'a str, RequestError> {
    uris.get(&reference).copied().ok_or_else(|| {
        RequestError::invalid(format!(
            "extension URI reference {reference} is not declared"
        ))
    })
}
