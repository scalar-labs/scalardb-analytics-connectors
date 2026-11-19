//! The type mapping: DynamoDB attribute types to Analytics types.
//!
//! It mirrors the "Type mapping" section of the connector's README. Data
//! Source types are the `AttributeType` of a key attribute's definition in
//! `DescribeTable`, which declares the types of key attributes only, and
//! [`ITEM`] for the column that holds each whole item.

use analytics_types::AnalyticsType;
use connector_sdk::{ColumnType, UnsupportedReason};

use super::ITEM;

/// Applies the type mapping to a column's Data Source type, or reports that
/// `DescribeTable` declared none for a key attribute.
pub(super) fn column_type(attribute_type: Option<&str>) -> ColumnType {
    let Some(attribute_type) = attribute_type else {
        return ColumnType::Unread {
            reason: "DescribeTable declared no type for the key attribute".to_owned(),
        };
    };
    let data_source_type = attribute_type.to_owned();
    match map(attribute_type) {
        Ok(analytics_type) => ColumnType::Mapped {
            data_source_type,
            analytics_type,
        },
        Err(reason) => ColumnType::Unsupported {
            data_source_type,
            reason,
        },
    }
}

/// One row of the type mapping per arm.
fn map(attribute_type: &str) -> Result<AnalyticsType, UnsupportedReason> {
    Ok(match attribute_type {
        "S" => AnalyticsType::Text,
        "B" => AnalyticsType::Blob,
        ITEM => AnalyticsType::Json,
        // A number holds up to 38 significant digits with an exponent from
        // -130 to +125, which no one decimal type holds.
        _ => return Err(UnsupportedReason::NotMapped),
    })
}
