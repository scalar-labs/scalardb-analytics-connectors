//! The Oracle type mapping (`crates/connectors/oracle/README.md#type-mapping`),
//! the spelling of Data Source types, and a column as table metadata states
//! it.
//!
//! A column's rule is looked up by `ALL_TAB_COLS.DATA_TYPE`, with its
//! parameters from the same row. An object type, such as `SDO_GEOMETRY` or a
//! user's own, has an owner and is not mapped.

use analytics_types::{AnalyticsType, DecimalType};
use connector_sdk::{Column, ColumnType, Reported, UnsupportedReason};

use super::OracleColumn;

/// The fractional second digits a timestamp can carry: microseconds.
const MAX_FRACTION_DIGITS: i64 = 6;

impl OracleColumn {
    /// The Data Source type of the column: an object type qualified with its
    /// owner, such as `MDSYS.SDO_GEOMETRY`, or else the built-in type with its
    /// parameters, such as `NUMBER(10,2)`, `NUMBER`, `NUMBER(*,0)`,
    /// `VARCHAR2(20 CHAR)`, `NVARCHAR2(20)`, `RAW(16)`, `FLOAT(126)`, or
    /// `TIMESTAMP(6) WITH TIME ZONE`.
    pub(crate) fn data_source_type(&self) -> String {
        let name = self.data_type.as_str();
        if let Some(owner) = &self.data_type_owner {
            return format!("{owner}.{name}");
        }
        match name {
            "NUMBER" => match (self.data_precision, self.data_scale) {
                (Some(precision), Some(scale)) => format!("NUMBER({precision},{scale})"),
                (None, Some(scale)) => format!("NUMBER(*,{scale})"),
                _ => name.to_owned(),
            },
            "FLOAT" => match self.data_precision {
                Some(precision) => format!("FLOAT({precision})"),
                None => name.to_owned(),
            },
            "CHAR" | "VARCHAR2" => {
                let unit = if self.char_used.as_deref() == Some("C") {
                    "CHAR"
                } else {
                    "BYTE"
                };
                format!("{name}({} {unit})", self.char_length)
            }
            "NCHAR" | "NVARCHAR2" => format!("{name}({})", self.char_length),
            "RAW" => format!("{name}({})", self.data_length),
            _ => name.to_owned(),
        }
    }

    /// The column as table metadata states it. Oracle has no column
    /// identifier that survives a dropped column, so none is offered.
    pub(crate) fn table_column(&self) -> Column {
        let data_source_type = self.data_source_type();
        let ty = match self.map_type() {
            Ok(analytics_type) => ColumnType::Mapped {
                data_source_type,
                analytics_type,
            },
            Err(reason) => ColumnType::Unsupported {
                data_source_type,
                reason,
            },
        };
        Column {
            name: self.name.clone(),
            ordinal: u32::try_from(self.column_id).expect("Oracle has at most 1000 columns"),
            nullable: self.nullable,
            source_id: Reported::NotOffered,
            ty,
        }
    }

    /// Applies the type mapping and the coverage of this connector version
    /// to the column.
    pub(crate) fn map_type(&self) -> Result<AnalyticsType, UnsupportedReason> {
        if self.data_type_owner.is_some() {
            return Err(UnsupportedReason::NotMapped);
        }
        let mapped = match self.data_type.as_str() {
            "NUMBER" => return self.decimal(),
            "BINARY_FLOAT" => AnalyticsType::Float,
            "BINARY_DOUBLE" => AnalyticsType::Double,
            "BOOLEAN" => AnalyticsType::Boolean,
            "CHAR" | "NCHAR" | "VARCHAR2" | "NVARCHAR2" => AnalyticsType::Text,
            "RAW" => AnalyticsType::Blob,
            // Mapped to Text and Blob, but values of unbounded length cannot
            // be read whole through `arrow-odbc`, which reads each column
            // into a buffer of a fixed length.
            "CLOB" | "NCLOB" | "BLOB" => return Err(UnsupportedReason::MappingNotImplemented),
            "DATE" => AnalyticsType::Timestamp,
            // `DATA_SCALE` is the fractional second precision.
            name if name.starts_with("TIMESTAMP(") => {
                if self
                    .data_scale
                    .is_some_and(|digits| digits > MAX_FRACTION_DIGITS)
                {
                    return Err(UnsupportedReason::ParameterOutOfRange);
                }
                if name.ends_with(" TIME ZONE") {
                    AnalyticsType::TimestampTz
                } else {
                    AnalyticsType::Timestamp
                }
            }
            _ => return Err(UnsupportedReason::NotMapped),
        };
        Ok(mapped)
    }

    /// `NUMBER(p, s)` is `Decimal(p, s)` when 0 <= s <= p. `NUMBER` without a
    /// precision, a negative scale, and a scale greater than the precision
    /// are outside the mappable range.
    fn decimal(&self) -> Result<AnalyticsType, UnsupportedReason> {
        let (Some(precision), Some(scale)) = (self.data_precision, self.data_scale) else {
            return Err(UnsupportedReason::ParameterOutOfRange);
        };
        if !(0..=precision).contains(&scale) {
            return Err(UnsupportedReason::ParameterOutOfRange);
        }
        let (Ok(precision), Ok(scale)) = (u8::try_from(precision), i8::try_from(scale)) else {
            return Err(UnsupportedReason::ParameterOutOfRange);
        };
        DecimalType::new(precision, scale)
            .map(AnalyticsType::Decimal)
            .map_err(|_| UnsupportedReason::ParameterOutOfRange)
    }
}
