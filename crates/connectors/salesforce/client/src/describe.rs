//! Describe results as Salesforce returns them.
//!
//! The client does not interpret them: a field's type stays the string
//! Salesforce reports, so a type the client has never seen still deserializes,
//! and the connector decides what each attribute means. Attributes that a
//! response omits take their default.

use serde::Deserialize;

/// The result of describeGlobal: the objects visible to the user
/// ([Describe Global](https://developer.salesforce.com/docs/platform/api-rest/guide/resources-describeglobal.html)).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GlobalDescribe {
    pub sobjects: Vec<SObjectSummary>,
}

/// One object in [`GlobalDescribe`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SObjectSummary {
    pub name: String,
    pub label: String,
    pub custom: bool,
    pub queryable: bool,
    pub retrieveable: bool,
    pub deprecated_and_hidden: bool,
}

/// The result of describing one object
/// ([sObject Describe](https://developer.salesforce.com/docs/platform/api-rest/guide/resources-sobject-describe.html)).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SObjectDescribe {
    pub name: String,
    pub label: String,
    pub custom: bool,
    pub queryable: bool,
    pub fields: Vec<FieldDescribe>,
}

/// One field of [`SObjectDescribe`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FieldDescribe {
    pub name: String,
    pub label: String,
    /// The field type as Salesforce reports it, such as `currency`
    /// ([Field Types](https://developer.salesforce.com/docs/atlas.en-us.api.meta/api/field_types.htm)).
    #[serde(rename = "type")]
    pub field_type: String,
    /// The SOAP type, such as `xsd:double`
    /// ([Primitive Data Types](https://developer.salesforce.com/docs/atlas.en-us.api.meta/api/primitive_data_types.htm)).
    pub soap_type: String,
    pub length: i64,
    pub byte_length: i64,
    pub precision: i64,
    pub scale: i64,
    pub digits: i64,
    pub nillable: bool,
    pub custom: bool,
    pub calculated: bool,
    pub case_sensitive: bool,
    pub filterable: bool,
    pub unique: bool,
    pub external_id: bool,
    pub id_lookup: bool,
    pub deprecated_and_hidden: bool,
    /// The objects a reference field can point to.
    pub reference_to: Vec<String>,
    pub relationship_name: Option<String>,
    /// The compound field this field is a component of, such as
    /// `BillingAddress`.
    pub compound_field_name: Option<String>,
}
