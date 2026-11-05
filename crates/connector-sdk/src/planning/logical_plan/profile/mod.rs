//! The Substrait profile accepted by the connector contract.

mod plan_extensions;

use prost::Message;
use prost_types::Any;
use substrait::proto::NamedStruct;

use super::expression::{BinaryOperator, UnaryOperator};

pub(in crate::planning::logical_plan) use plan_extensions::PlanExtensions;

/// Substrait comparison-function declarations accepted by the connector profile.
pub const COMPARISON_FUNCTIONS_URI: &str =
    "https://github.com/substrait-io/substrait/blob/v0.69.0/extensions/functions_comparison.yaml";
/// Substrait Boolean-function declarations accepted by the connector profile.
pub const BOOLEAN_FUNCTIONS_URI: &str =
    "https://github.com/substrait-io/substrait/blob/v0.69.0/extensions/functions_boolean.yaml";
/// ScalarDB Analytics logical-type declarations accepted by the connector profile.
pub const LOGICAL_TYPE_URI: &str = "urn:scalardb:analytics:substrait:logical";
/// Substrait extension type used for a remote result consumed by a local plan.
pub const REMOTE_RESULT_TYPE_URL: &str = "urn:scalardb:analytics:substrait:remote-result-schema:v1";

/// One declared scalar function with its URI anchor resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::planning::logical_plan) struct FunctionDeclaration<'a> {
    pub uri: &'a str,
    pub name: &'a str,
}

/// The connector contract's static set of extension URIs and their declarations.
struct ConnectorProfile {
    extensions: &'static [ExtensionProfile],
}

/// One extension URI paired with all declarations recognized under it.
struct ExtensionProfile {
    uri: &'static str,
    declarations: &'static [ProfileDeclaration],
}

/// One declaration recognized by the connector profile.
enum ProfileDeclaration {
    Function {
        name: &'static str,
        function: ProfileFunction,
    },
    Type {
        name: &'static str,
        data_type: ProfileType,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::planning::logical_plan) enum ProfileFunction {
    Unary(UnaryOperator),
    Binary(BinaryOperator),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProfileType {
    Logical,
}

const COMPARISON_DECLARATIONS: &[ProfileDeclaration] = &[
    ProfileDeclaration::Function {
        name: "equal:any1_any1",
        function: ProfileFunction::Binary(BinaryOperator::Equal),
    },
    ProfileDeclaration::Function {
        name: "not_equal:any1_any1",
        function: ProfileFunction::Binary(BinaryOperator::NotEqual),
    },
    ProfileDeclaration::Function {
        name: "lt:any1_any1",
        function: ProfileFunction::Binary(BinaryOperator::LessThan),
    },
    ProfileDeclaration::Function {
        name: "lte:any1_any1",
        function: ProfileFunction::Binary(BinaryOperator::LessThanOrEqual),
    },
    ProfileDeclaration::Function {
        name: "gt:any1_any1",
        function: ProfileFunction::Binary(BinaryOperator::GreaterThan),
    },
    ProfileDeclaration::Function {
        name: "gte:any1_any1",
        function: ProfileFunction::Binary(BinaryOperator::GreaterThanOrEqual),
    },
    ProfileDeclaration::Function {
        name: "is_null:any",
        function: ProfileFunction::Unary(UnaryOperator::IsNull),
    },
    ProfileDeclaration::Function {
        name: "is_not_null:any",
        function: ProfileFunction::Unary(UnaryOperator::IsNotNull),
    },
];

const BOOLEAN_DECLARATIONS: &[ProfileDeclaration] = &[
    ProfileDeclaration::Function {
        name: "and:bool_bool",
        function: ProfileFunction::Binary(BinaryOperator::And),
    },
    ProfileDeclaration::Function {
        name: "or:bool_bool",
        function: ProfileFunction::Binary(BinaryOperator::Or),
    },
    ProfileDeclaration::Function {
        name: "not:bool",
        function: ProfileFunction::Unary(UnaryOperator::Not),
    },
];

const LOGICAL_TYPE_DECLARATIONS: &[ProfileDeclaration] = &[ProfileDeclaration::Type {
    name: "logical",
    data_type: ProfileType::Logical,
}];

const CONNECTOR_PROFILE: ConnectorProfile = ConnectorProfile {
    extensions: &[
        ExtensionProfile {
            uri: COMPARISON_FUNCTIONS_URI,
            declarations: COMPARISON_DECLARATIONS,
        },
        ExtensionProfile {
            uri: BOOLEAN_FUNCTIONS_URI,
            declarations: BOOLEAN_DECLARATIONS,
        },
        ExtensionProfile {
            uri: LOGICAL_TYPE_URI,
            declarations: LOGICAL_TYPE_DECLARATIONS,
        },
    ],
};

impl ConnectorProfile {
    fn function(&self, uri: &str, name: &str) -> Option<ProfileFunction> {
        self.declarations(uri)
            .find_map(|declaration| match declaration {
                ProfileDeclaration::Function {
                    name: declared_name,
                    function,
                } if *declared_name == name => Some(*function),
                _ => None,
            })
    }

    fn data_type(&self, uri: &str, name: &str) -> Option<ProfileType> {
        self.declarations(uri)
            .find_map(|declaration| match declaration {
                ProfileDeclaration::Type {
                    name: declared_name,
                    data_type,
                } if *declared_name == name => Some(*data_type),
                _ => None,
            })
    }

    fn has_type_name(&self, name: &str) -> bool {
        self.extensions.iter().any(|extension| {
            extension.declarations.iter().any(|declaration| {
                matches!(declaration, ProfileDeclaration::Type { name: declared_name, .. } if *declared_name == name)
            })
        })
    }

    fn declarations(&self, uri: &str) -> impl Iterator<Item = &'static ProfileDeclaration> {
        self.extensions
            .iter()
            .find(|extension| extension.uri == uri)
            .into_iter()
            .flat_map(|extension| extension.declarations)
    }
}

/// Declares the boundary type URL required by a plan containing `RemoteResult`.
pub(super) fn remote_result_expected_type_urls(contains_remote_result: bool) -> Vec<String> {
    if contains_remote_result {
        vec![REMOTE_RESULT_TYPE_URL.to_owned()]
    } else {
        Vec::new()
    }
}

/// Encodes a `RemoteResult` schema as its declared Substrait extension payload.
pub(super) fn remote_result_detail(schema: NamedStruct) -> Any {
    Any {
        type_url: REMOTE_RESULT_TYPE_URL.to_owned(),
        value: schema.encode_to_vec(),
    }
}
