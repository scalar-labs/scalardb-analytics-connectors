//! The PostgreSQL type mapping (`docs/type-mappings.md#postgresql`).

use analytics_types::{AnalyticsType, DecimalType, EnumType};
use connector_sdk::UnsupportedReason;
use serde::{Deserialize, Serialize};

/// The type a column's mapping rule is looked up by: the column's own type, or
/// for a domain the base type it resolves to through nested domains.
///
/// `format_type` alone cannot identify the rule for a domain or an enum, which
/// it reports by name only, so the catalog query supplies these facts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseType {
    /// The schema of the type, `pg_catalog` for built-in types.
    pub schema: String,
    /// `pg_type.typname`, such as `int4` or `timestamptz`.
    pub name: String,
    /// `pg_type.typtype`: `b` base, `e` enum, `c` composite, `r` range, and so on.
    pub kind: String,
    /// `pg_type.typcategory`; `A` marks array types.
    pub category: String,
    /// The type modifier in effect, from the column or the innermost domain
    /// that declares one; `-1` when none is declared.
    pub typmod: i32,
    /// Member labels in sort order, for an enum.
    pub enum_labels: Option<Vec<String>>,
}

/// Applies the type mapping to one column's base type.
pub(crate) fn map_type(base: &BaseType) -> Result<AnalyticsType, UnsupportedReason> {
    if base.kind == "e" {
        let labels = base.enum_labels.clone().unwrap_or_default();
        return Ok(AnalyticsType::Enum(EnumType::new(labels)));
    }
    if base.schema != "pg_catalog" || base.kind != "b" || base.category == "A" {
        return Err(UnsupportedReason::NotMapped);
    }
    let mapped = match base.name.as_str() {
        "bool" => AnalyticsType::Boolean,
        "int2" => AnalyticsType::SmallInt,
        "int4" => AnalyticsType::Int,
        "int8" => AnalyticsType::BigInt,
        "float4" => AnalyticsType::Float,
        "float8" => AnalyticsType::Double,
        "numeric" => return numeric(base.typmod),
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

    fn builtin(name: &str, typmod: i32) -> BaseType {
        BaseType {
            schema: "pg_catalog".to_owned(),
            name: name.to_owned(),
            kind: "b".to_owned(),
            category: "N".to_owned(),
            typmod,
            enum_labels: None,
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
        let array = BaseType {
            category: "A".to_owned(),
            ..builtin("_int4", -1)
        };
        assert_eq!(map_type(&array), Err(UnsupportedReason::NotMapped));
        let extension = BaseType {
            schema: "public".to_owned(),
            ..builtin("citext", -1)
        };
        assert_eq!(map_type(&extension), Err(UnsupportedReason::NotMapped));
    }

    #[test]
    fn enums_map_with_their_members_in_sort_order() {
        let base = BaseType {
            schema: "public".to_owned(),
            name: "mood".to_owned(),
            kind: "e".to_owned(),
            category: "E".to_owned(),
            typmod: -1,
            enum_labels: Some(vec!["sad".to_owned(), "happy".to_owned()]),
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
