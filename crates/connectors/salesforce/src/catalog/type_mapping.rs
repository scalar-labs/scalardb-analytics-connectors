//! The type mapping: describe field types to Analytics types.
//!
//! It mirrors the "Type mapping" section of the connector's README. Data
//! Source types are the `type` attribute of a field's describe result,
//! followed by `(precision,scale)` for the numeric types that carry them.

use analytics_types::{AnalyticsType, DecimalType};
use connector_sdk::{ColumnType, UnsupportedReason};

use super::SalesforceField;

impl SalesforceField {
    /// The field's Data Source type as table metadata spells it.
    pub fn data_source_type(&self) -> String {
        if has_precision(&self.field_type) {
            format!("{}({},{})", self.field_type, self.precision, self.scale)
        } else {
            self.field_type.clone()
        }
    }

    /// Applies the type mapping to the field.
    pub fn column_type(&self) -> ColumnType {
        let data_source_type = self.data_source_type();
        match map(&self.field_type, self.precision, self.scale) {
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

    /// The field's Analytics type, when the type mapping maps it.
    pub fn analytics_type(&self) -> Option<AnalyticsType> {
        map(&self.field_type, self.precision, self.scale).ok()
    }
}

/// Whether the type's describe result carries a precision and scale that
/// define its values.
fn has_precision(field_type: &str) -> bool {
    matches!(
        field_type.to_ascii_lowercase().as_str(),
        "double" | "currency" | "percent"
    )
}

/// One row of the type mapping per arm; a type matches case-insensitively.
fn map(field_type: &str, precision: i64, scale: i64) -> Result<AnalyticsType, UnsupportedReason> {
    Ok(match field_type.to_ascii_lowercase().as_str() {
        "id" | "reference" | "string" | "textarea" | "picklist" | "multipicklist" | "combobox"
        | "email" | "phone" | "url" | "encryptedstring" => AnalyticsType::Text,
        "boolean" => AnalyticsType::Boolean,
        "int" => AnalyticsType::Int,
        "long" => AnalyticsType::BigInt,
        "double" | "currency" | "percent" => AnalyticsType::Decimal(decimal(precision, scale)?),
        "date" => AnalyticsType::Date,
        "datetime" => AnalyticsType::TimestampTz,
        "time" => AnalyticsType::Time,
        // base64 values come back as the URL of the content, address and
        // location are compounds of fields that are also listed, and
        // anyType values change type from row to row.
        _ => return Err(UnsupportedReason::NotMapped),
    })
}

/// The most digits of a decimal the client decodes, into Arrow `Decimal128`.
const MAX_DECODED_PRECISION: u8 = 38;

/// `Decimal(precision, scale)` when both fit the Analytics decimal and the
/// client decodes it.
fn decimal(precision: i64, scale: i64) -> Result<DecimalType, UnsupportedReason> {
    let out_of_range = || UnsupportedReason::ParameterOutOfRange;
    let precision = u8::try_from(precision).map_err(|_| out_of_range())?;
    let scale = i8::try_from(scale).map_err(|_| out_of_range())?;
    if scale < 0 {
        return Err(out_of_range());
    }
    let decimal = DecimalType::new(precision, scale).map_err(|_| out_of_range())?;
    // The type mapping maps these, but the client has no `Decimal256`.
    if precision > MAX_DECODED_PRECISION {
        return Err(UnsupportedReason::MappingNotImplemented);
    }
    Ok(decimal)
}
