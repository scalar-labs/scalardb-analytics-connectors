//! The SQL Server type mapping (`crates/connectors/sqlserver-odbc/README.md#type-mapping`)
//! and the spelling of Data Source types.
//!
//! A column's rule is looked up by its system type: the column's own type, or
//! for an alias type such as `sysname` the system type it is created from.
//! Parameters come from `sys.columns`, which states them for alias types too.

use analytics_types::{AnalyticsType, DecimalType};
use connector_sdk::UnsupportedReason;

use super::{SqlServerColumn, SqlServerType};

/// The `max_length` of a `(max)` type.
const MAX: i16 = -1;

impl SqlServerColumn {
    /// The Data Source type of the column, of type `ty`: the name of a type a
    /// user created, qualified with its schema, or of a system alias type
    /// such as `sysname`, or else the system type with its parameters,
    /// such as `nvarchar(50)`, `varbinary(max)`, `decimal(10,2)`, or
    /// `datetime2(7)`.
    pub(crate) fn data_source_type(&self, ty: &SqlServerType) -> String {
        if ty.user_defined {
            return format!("{}.{}", ty.schema, ty.name);
        }
        if ty.system_type.as_deref() != Some(ty.name.as_str()) {
            return ty.name.clone();
        }
        let name = ty.name.as_str();
        let length = |bytes_per_character: i16| {
            if self.max_length == MAX {
                "max".to_owned()
            } else {
                (self.max_length / bytes_per_character).to_string()
            }
        };
        match name {
            "char" | "varchar" | "binary" | "varbinary" => format!("{name}({})", length(1)),
            "nchar" | "nvarchar" => format!("{name}({})", length(2)),
            "decimal" | "numeric" => format!("{name}({},{})", self.precision, self.scale),
            "time" | "datetime2" | "datetimeoffset" => format!("{name}({})", self.scale),
            _ => name.to_owned(),
        }
    }

    /// Applies the type mapping and the coverage of this connector version
    /// to the column, of type `ty`.
    pub(crate) fn map_type(&self, ty: &SqlServerType) -> Result<AnalyticsType, UnsupportedReason> {
        let Some(system_type) = ty.system_type.as_deref() else {
            return Err(UnsupportedReason::NotMapped);
        };
        let unbounded = self.max_length == MAX;
        let mapped = match system_type {
            "bit" => AnalyticsType::Boolean,
            "tinyint" => AnalyticsType::UnsignedByte,
            "smallint" => AnalyticsType::SmallInt,
            "int" => AnalyticsType::Int,
            "bigint" => AnalyticsType::BigInt,
            "real" => AnalyticsType::Float,
            "float" => AnalyticsType::Double,
            "decimal" | "numeric" => return decimal(self.precision, self.scale),
            "money" => return decimal(19, 4),
            "smallmoney" => return decimal(10, 4),
            "char" | "varchar" | "nchar" | "nvarchar" if !unbounded => AnalyticsType::Text,
            "binary" | "varbinary" | "timestamp" if !unbounded => AnalyticsType::Blob,
            // Mapped to Text and Blob, but values of unbounded length cannot
            // be read whole through `arrow-odbc`, which reads each column
            // into a buffer of a fixed length.
            "varchar" | "nvarchar" | "text" | "ntext" | "xml" | "varbinary" | "image" => {
                return Err(UnsupportedReason::MappingNotImplemented);
            }
            "date" => AnalyticsType::Date,
            "time" => AnalyticsType::Time,
            "datetime" | "smalldatetime" | "datetime2" => AnalyticsType::Timestamp,
            "datetimeoffset" => AnalyticsType::TimestampTz,
            "uniqueidentifier" => AnalyticsType::Uuid,
            _ => return Err(UnsupportedReason::NotMapped),
        };
        Ok(mapped)
    }
}

/// `Decimal(precision, scale)`; SQL Server's precision is at most 38.
fn decimal(precision: u8, scale: u8) -> Result<AnalyticsType, UnsupportedReason> {
    let scale = i8::try_from(scale).map_err(|_| UnsupportedReason::ParameterOutOfRange)?;
    DecimalType::new(precision, scale)
        .map(AnalyticsType::Decimal)
        .map_err(|_| UnsupportedReason::ParameterOutOfRange)
}
