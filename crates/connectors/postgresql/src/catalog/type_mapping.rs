//! The PostgreSQL type mapping (`crates/connectors/postgresql/README.md#type-mapping`).

use analytics_types::{AnalyticsType, DecimalType, EnumType};
use connector_sdk::UnsupportedReason;

/// The type a column's mapping rule is looked up by: the column's own type, or
/// for a domain the type it resolves to through nested domains.
///
/// `format_type` alone cannot identify the rule for a domain or an enum, which
/// it reports by name only, so it is resolved from the source metadata's
/// types ([`PostgresTable::resolve_type`](super::PostgresTable::resolve_type)).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BaseType {
    /// A base type, built in or from an extension.
    Base {
        /// The schema of the type, `pg_catalog` for built-in types.
        schema: String,
        /// `pg_type.typname`, such as `int4` or `timestamptz`.
        typname: String,
        /// The type modifier in effect, from the column or the innermost
        /// domain that declares one; `-1` when none is declared.
        typmod: i32,
    },
    /// An enum, with its labels in sort order.
    Enum { labels: Vec<String> },
    /// An array, composite, range, multirange, or pseudo-type, which the
    /// mapping does not map.
    Unmapped,
}

/// Applies the type mapping to one column's base type.
pub(crate) fn map_type(base: &BaseType) -> Result<AnalyticsType, UnsupportedReason> {
    let (typname, typmod) = match base {
        BaseType::Enum { labels } => return Ok(AnalyticsType::Enum(EnumType::new(labels.clone()))),
        BaseType::Base {
            schema,
            typname,
            typmod,
        } if schema == "pg_catalog" => (typname, *typmod),
        BaseType::Base { .. } | BaseType::Unmapped => return Err(UnsupportedReason::NotMapped),
    };
    let mapped = match typname.as_str() {
        "bool" => AnalyticsType::Boolean,
        "int2" => AnalyticsType::SmallInt,
        "int4" => AnalyticsType::Int,
        "int8" => AnalyticsType::BigInt,
        "float4" => AnalyticsType::Float,
        "float8" => AnalyticsType::Double,
        "numeric" => return numeric(typmod),
        "text" | "varchar" | "bpchar" => AnalyticsType::Text,
        "bytea" => AnalyticsType::Blob,
        "date" => AnalyticsType::Date,
        "time" => AnalyticsType::Time,
        "timestamp" => AnalyticsType::Timestamp,
        "timestamptz" => AnalyticsType::TimestampTz,
        "interval" => AnalyticsType::Interval,
        "uuid" => AnalyticsType::Uuid,
        "json" | "jsonb" => AnalyticsType::Json,
        "timetz" => AnalyticsType::TimeWithTimeZone,
        _ => return Err(UnsupportedReason::NotMapped),
    };
    Ok(mapped)
}

/// Maps `numeric(p, s)` to `Decimal(p, s)` when it is admitted.
///
/// PostgreSQL stores `((p << 16) | s) + 4` in the type modifier, with the
/// scale as an 11-bit two's-complement value since PostgreSQL 15. `numeric`
/// without a precision, a precision above 38, and a scale greater than the
/// precision are outside the mappable range.
fn numeric(typmod: i32) -> Result<AnalyticsType, UnsupportedReason> {
    if typmod < 4 {
        return Err(UnsupportedReason::ParameterOutOfRange);
    }
    let packed = typmod - 4;
    let precision = (packed >> 16) & 0xffff;
    let scale = ((packed & 0x7ff) ^ 0x400) - 0x400;
    let (Ok(precision), Ok(scale)) = (u8::try_from(precision), i8::try_from(scale)) else {
        return Err(UnsupportedReason::ParameterOutOfRange);
    };
    DecimalType::new(precision, scale)
        .map(AnalyticsType::Decimal)
        .map_err(|_| UnsupportedReason::ParameterOutOfRange)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtin(typname: &str, typmod: i32) -> BaseType {
        BaseType::Base {
            schema: "pg_catalog".to_owned(),
            typname: typname.to_owned(),
            typmod,
        }
    }

    fn numeric_typmod(precision: i32, scale: i32) -> i32 {
        ((precision << 16) | (scale & 0x7ff)) + 4
    }

    #[test]
    fn numeric_maps_within_the_decimal_range() {
        let decimal = |precision, scale| {
            Ok(AnalyticsType::Decimal(
                DecimalType::new(precision, scale).unwrap(),
            ))
        };
        assert_eq!(
            map_type(&builtin("numeric", numeric_typmod(10, 2))),
            decimal(10, 2)
        );
        assert_eq!(
            map_type(&builtin("numeric", numeric_typmod(38, 0))),
            decimal(38, 0)
        );
        assert_eq!(
            map_type(&builtin("numeric", numeric_typmod(5, -2))),
            decimal(5, -2)
        );
        for typmod in [-1, numeric_typmod(39, 0), numeric_typmod(2, 5)] {
            assert_eq!(
                map_type(&builtin("numeric", typmod)),
                Err(UnsupportedReason::ParameterOutOfRange)
            );
        }
    }

    #[test]
    fn types_outside_the_mapping_are_not_mapped() {
        assert_eq!(
            map_type(&builtin("money", -1)),
            Err(UnsupportedReason::NotMapped)
        );
        assert_eq!(
            map_type(&BaseType::Unmapped),
            Err(UnsupportedReason::NotMapped)
        );
        let extension = BaseType::Base {
            schema: "public".to_owned(),
            typname: "citext".to_owned(),
            typmod: -1,
        };
        assert_eq!(map_type(&extension), Err(UnsupportedReason::NotMapped));
    }

    #[test]
    fn enums_map_with_their_members_in_sort_order() {
        let base = BaseType::Enum {
            labels: vec!["sad".to_owned(), "happy".to_owned()],
        };
        assert_eq!(
            map_type(&base),
            Ok(AnalyticsType::Enum(EnumType::new(vec![
                "sad".to_owned(),
                "happy".to_owned()
            ])))
        );
    }
}
