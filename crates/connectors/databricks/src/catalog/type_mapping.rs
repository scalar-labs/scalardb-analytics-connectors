//! The Databricks type mapping
//! (`crates/connectors/databricks/README.md#type-mapping`).

use analytics_types::{AnalyticsField, AnalyticsType, ArrayType, DecimalType, MapType, StructType};
use connector_sdk::UnsupportedReason;

use super::data_type::DataType;

/// The fractional digits of the one `timestamp_ntz` the mapping maps,
/// microseconds.
const MICROSECOND_DIGITS: u8 = 6;

/// Maps one column's `full_data_type`.
///
/// Text that is no type the grammar reads is not mapped.
pub(crate) fn map_full_data_type(text: &str) -> Result<AnalyticsType, UnsupportedReason> {
    DataType::parse(text)
        .as_ref()
        .map_or(Err(UnsupportedReason::NotMapped), map_type)
}

/// Applies the type mapping to one Databricks type.
///
/// Every string type is `Text`, whatever its length or collation. `timestamp`
/// is an instant and `TimestampTz`; `timestamp_ntz` is `Timestamp`. An array,
/// map, or struct maps when every type inside it maps; its elements, values,
/// and fields are nullable, because `full_data_type` does not state their
/// nullability. Intervals, `variant`, spatial types, and the rest are not
/// mapped.
pub(crate) fn map_type(data_type: &DataType) -> Result<AnalyticsType, UnsupportedReason> {
    Ok(match data_type {
        DataType::Boolean => AnalyticsType::Boolean,
        DataType::TinyInt => AnalyticsType::Byte,
        DataType::SmallInt => AnalyticsType::SmallInt,
        DataType::Int => AnalyticsType::Int,
        DataType::BigInt => AnalyticsType::BigInt,
        DataType::Float => AnalyticsType::Float,
        DataType::Double => AnalyticsType::Double,
        DataType::Decimal { precision, scale } => {
            let scale = i8::try_from(*scale).map_err(|_| UnsupportedReason::ParameterOutOfRange)?;
            AnalyticsType::Decimal(
                DecimalType::new(*precision, scale)
                    .map_err(|_| UnsupportedReason::ParameterOutOfRange)?,
            )
        }
        DataType::String { .. } | DataType::Varchar { .. } | DataType::Char { .. } => {
            AnalyticsType::Text
        }
        DataType::Binary => AnalyticsType::Blob,
        DataType::Date => AnalyticsType::Date,
        DataType::Timestamp => AnalyticsType::TimestampTz,
        DataType::TimestampNtz { precision } if *precision == MICROSECOND_DIGITS => {
            AnalyticsType::Timestamp
        }
        DataType::TimestampNtz { .. } => return Err(UnsupportedReason::ParameterOutOfRange),
        DataType::Array(element) => AnalyticsType::Array(ArrayType::new(map_type(element)?, true)),
        DataType::Map { key, value } => {
            AnalyticsType::Map(MapType::new(map_type(key)?, map_type(value)?, true))
        }
        DataType::Struct(fields) => AnalyticsType::Struct(StructType::new(
            fields
                .iter()
                .map(|field| {
                    Ok(AnalyticsField::new(
                        field.name.clone(),
                        map_type(&field.data_type)?,
                        true,
                    ))
                })
                .collect::<Result<_, _>>()?,
        )),
        DataType::Other(_) => return Err(UnsupportedReason::NotMapped),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapped(text: &str) -> AnalyticsType {
        map_full_data_type(text).unwrap_or_else(|reason| panic!("`{text}`: {reason:?}"))
    }

    fn decimal(precision: u8, scale: i8) -> AnalyticsType {
        AnalyticsType::Decimal(DecimalType::new(precision, scale).unwrap())
    }

    #[test]
    fn scalar_types_map_by_the_table() {
        for (text, expected) in [
            ("boolean", AnalyticsType::Boolean),
            ("tinyint", AnalyticsType::Byte),
            ("smallint", AnalyticsType::SmallInt),
            ("int", AnalyticsType::Int),
            ("bigint", AnalyticsType::BigInt),
            ("float", AnalyticsType::Float),
            ("double", AnalyticsType::Double),
            ("decimal(1,0)", decimal(1, 0)),
            ("decimal(10,0)", decimal(10, 0)),
            ("decimal(18,4)", decimal(18, 4)),
            ("decimal(38,38)", decimal(38, 38)),
            ("string", AnalyticsType::Text),
            ("string collate UTF8_LCASE", AnalyticsType::Text),
            ("varchar(10)", AnalyticsType::Text),
            ("char(3)", AnalyticsType::Text),
            ("binary", AnalyticsType::Blob),
            ("date", AnalyticsType::Date),
            ("timestamp", AnalyticsType::TimestampTz),
            ("timestamp_ntz", AnalyticsType::Timestamp),
            ("timestamp_ntz(6)", AnalyticsType::Timestamp),
        ] {
            assert_eq!(mapped(text), expected, "{text}");
        }
    }

    #[test]
    fn integral_decimals_stay_decimals() {
        assert_eq!(mapped("decimal(19,0)"), decimal(19, 0));
        assert_eq!(mapped("decimal(5,0)"), decimal(5, 0));
    }

    #[test]
    fn nested_types_map_when_everything_inside_maps() {
        assert_eq!(
            mapped("array<int>"),
            AnalyticsType::Array(ArrayType::new(AnalyticsType::Int, true))
        );
        assert_eq!(
            mapped("map<string,array<decimal(5,2)>>"),
            AnalyticsType::Map(MapType::new(
                AnalyticsType::Text,
                AnalyticsType::Array(ArrayType::new(decimal(5, 2), true)),
                true
            ))
        );
        assert_eq!(
            mapped("struct<id:bigint,`at time`:timestamp,tags:array<string>>"),
            AnalyticsType::Struct(StructType::new(vec![
                AnalyticsField::new("id", AnalyticsType::BigInt, true),
                AnalyticsField::new("at time", AnalyticsType::TimestampTz, true),
                AnalyticsField::new(
                    "tags",
                    AnalyticsType::Array(ArrayType::new(AnalyticsType::Text, true)),
                    true
                ),
            ]))
        );
        assert_eq!(
            mapped("struct<>"),
            AnalyticsType::Struct(StructType::new(Vec::new()))
        );
    }

    #[test]
    fn a_nested_type_with_an_unmapped_type_inside_is_unmapped() {
        for (text, reason) in [
            ("array<variant>", UnsupportedReason::NotMapped),
            (
                "map<string,interval day to second>",
                UnsupportedReason::NotMapped,
            ),
            (
                "struct<a:int,b:struct<c:geography(4326)>>",
                UnsupportedReason::NotMapped,
            ),
            (
                "array<timestamp_ntz(9)>",
                UnsupportedReason::ParameterOutOfRange,
            ),
        ] {
            assert_eq!(map_full_data_type(text), Err(reason), "{text}");
        }
    }

    #[test]
    fn types_outside_the_mapping_are_not_mapped() {
        for text in [
            "interval year to month",
            "interval day to second",
            "variant",
            "geography(4326)",
            "geometry(0)",
            "void",
            "object",
            "time",
            "decfloat",
        ] {
            assert_eq!(
                map_full_data_type(text),
                Err(UnsupportedReason::NotMapped),
                "{text}"
            );
        }
    }

    #[test]
    fn out_of_range_parameters_are_reported_as_such() {
        for text in [
            "timestamp_ntz(7)",
            "timestamp_ntz(9)",
            "decimal(0,0)",
            "decimal(5,6)",
        ] {
            assert_eq!(
                map_full_data_type(text),
                Err(UnsupportedReason::ParameterOutOfRange),
                "{text}"
            );
        }
    }

    #[test]
    fn unreadable_text_is_not_mapped() {
        assert_eq!(
            map_full_data_type("array<int"),
            Err(UnsupportedReason::NotMapped)
        );
    }
}
