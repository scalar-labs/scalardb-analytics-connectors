//! Converting the ADBC Databricks driver's output to Analytics values.
//!
//! The driver passes on the Arrow stream Databricks sends, which it asks for
//! with native Arrow decimals, timestamps, and complex types. Most mapped
//! types arrive in their Analytics storage type and pass through: integers,
//! floating-point numbers, `boolean`, `string`, `binary`, and `date`. The rest
//! are converted without changing any value:
//!
//! | Mapping rule | Driver output | Conversion |
//! | --- | --- | --- |
//! | `decimal(p,s)` | `Decimal128` of scale s | To the narrowest Arrow decimal of precision p; a value with more than p digits is a `data` error |
//! | `timestamp` | `Timestamp` with the session's time zone | Aligned to microseconds and labelled `UTC`; the instant is unchanged |
//! | `timestamp_ntz` | `Timestamp` without a time zone | Aligned to microseconds |
//! | `array<T>`, `map<K,V>`, `struct<...>` | `List`, `Map`, `Struct` | Rebuilt with the Analytics fields, each element, key, value, or field converted by its own rule |
//!
//! These driver output types are what Databricks documents for its Arrow
//! results and have not been read from a workspace. A column in any other
//! form fails the read as `internal`, naming the type that arrived.

use std::sync::Arc;

use analytics_types::{AnalyticsType, DecimalType as AnalyticsDecimal, arrow_schema::DataType};
use arrow_array::{
    Array, ArrayRef, Decimal128Array, ListArray, MapArray, PrimitiveArray, StructArray,
    types::{Decimal32Type, Decimal64Type, Decimal128Type, DecimalType},
};
use connector_sdk::{
    ConnectorError, ErrorKind,
    convert::{downcast, not_carried, timestamps_to_micros},
};

/// Converts one column of the driver's output to `target`'s Analytics storage.
pub(crate) fn convert_column(
    column: &ArrayRef,
    target: &AnalyticsType,
) -> Result<ArrayRef, ConnectorError> {
    let storage = target.storage_type();
    if column.data_type() == &storage {
        return Ok(Arc::clone(column));
    }
    match (column.data_type(), target) {
        (DataType::Decimal128(_, scale), AnalyticsType::Decimal(decimal))
            if *scale == decimal.scale() =>
        {
            decimals(downcast::<Decimal128Array>(column)?, *decimal)
        }
        (DataType::Timestamp(_, Some(_)), AnalyticsType::TimestampTz) => {
            Ok(Arc::new(timestamps_to_micros(column)?.with_timezone("UTC")))
        }
        (DataType::Timestamp(_, None), AnalyticsType::Timestamp) => {
            Ok(Arc::new(timestamps_to_micros(column)?))
        }
        (DataType::List(_), AnalyticsType::Array(array)) => {
            let lists = downcast::<ListArray>(column)?;
            let values = convert_column(lists.values(), array.element_type())?;
            let DataType::List(element) = storage else {
                return Err(unexpected(column, target));
            };
            ListArray::try_new(
                element,
                lists.offsets().clone(),
                values,
                lists.nulls().cloned(),
            )
            .map(|array| Arc::new(array) as ArrayRef)
            .map_err(|_| unexpected(column, target))
        }
        (DataType::Map(..), AnalyticsType::Map(map)) => {
            let maps = downcast::<MapArray>(column)?;
            let keys = convert_column(maps.keys(), map.key_type())?;
            let values = convert_column(maps.values(), map.value_type())?;
            let DataType::Map(entries, ordered) = storage else {
                return Err(unexpected(column, target));
            };
            let DataType::Struct(entry_fields) = entries.data_type().clone() else {
                return Err(unexpected(column, target));
            };
            let rebuilt = StructArray::try_new(
                entry_fields,
                vec![keys, values],
                maps.entries().nulls().cloned(),
            )
            .and_then(|rebuilt| {
                MapArray::try_new(
                    entries,
                    maps.offsets().clone(),
                    rebuilt,
                    maps.nulls().cloned(),
                    ordered,
                )
            });
            rebuilt
                .map(|array| Arc::new(array) as ArrayRef)
                .map_err(|_| unexpected(column, target))
        }
        (DataType::Struct(fields), AnalyticsType::Struct(struct_type))
            if fields.len() == struct_type.fields().len()
                && fields
                    .iter()
                    .zip(struct_type.fields())
                    .all(|(arrived, field)| arrived.name() == field.name()) =>
        {
            let structs = downcast::<StructArray>(column)?;
            let columns = structs
                .columns()
                .iter()
                .zip(struct_type.fields())
                .map(|(child, field)| convert_column(child, field.data_type()))
                .collect::<Result<Vec<_>, _>>()?;
            let DataType::Struct(target_fields) = storage else {
                return Err(unexpected(column, target));
            };
            StructArray::try_new(target_fields, columns, structs.nulls().cloned())
                .map(|array| Arc::new(array) as ArrayRef)
                .map_err(|_| unexpected(column, target))
        }
        _ => Err(unexpected(column, target)),
    }
}

/// Narrows `Decimal128` values of `decimal`'s scale to `decimal`'s storage,
/// checking that each has at most its precision of digits.
fn decimals(
    values: &Decimal128Array,
    decimal: AnalyticsDecimal,
) -> Result<ArrayRef, ConnectorError> {
    let precision = decimal.precision();
    let limit = 10_i128.pow(u32::from(precision));
    let target = || format!("Decimal({precision}, {})", decimal.scale());
    let checked = values
        .iter()
        .map(|value| {
            value
                .map(|value| {
                    (value.unsigned_abs() < limit.unsigned_abs())
                        .then_some(value)
                        .ok_or_else(|| not_carried(&target()))
                })
                .transpose()
        })
        .collect::<Result<Vec<_>, _>>()?;
    match AnalyticsType::Decimal(decimal).storage_type() {
        DataType::Decimal32(..) => narrowed::<Decimal32Type>(&checked, decimal),
        DataType::Decimal64(..) => narrowed::<Decimal64Type>(&checked, decimal),
        _ => narrowed::<Decimal128Type>(&checked, decimal),
    }
}

/// Builds a decimal array of width `T` from unscaled values that already fit
/// `decimal`'s precision.
fn narrowed<T: DecimalType>(
    values: &[Option<i128>],
    decimal: AnalyticsDecimal,
) -> Result<ArrayRef, ConnectorError>
where
    T::Native: TryFrom<i128>,
{
    let internal = || {
        ConnectorError::new(
            ErrorKind::Internal,
            false,
            "a decimal within its precision did not fit its storage",
        )
    };
    let array = values
        .iter()
        .map(|value| {
            value
                .map(|value| T::Native::try_from(value).map_err(|_| internal()))
                .transpose()
        })
        .collect::<Result<PrimitiveArray<T>, _>>()?
        .with_precision_and_scale(decimal.precision(), decimal.scale())
        .map_err(|_| internal())?;
    Ok(Arc::new(array))
}

/// The error for a column that arrived in a form the connector does not
/// convert to `target`, naming both types and no value.
fn unexpected(column: &ArrayRef, target: &AnalyticsType) -> ConnectorError {
    ConnectorError::new(
        ErrorKind::Internal,
        false,
        format!(
            "the Databricks driver returned a {} column where {} storage was expected",
            column.data_type(),
            target.storage_type()
        ),
    )
}

#[cfg(test)]
mod tests {
    use analytics_types::{AnalyticsField, ArrayType, MapType, StructType, arrow_schema::Field};
    use arrow_array::{
        Decimal32Array, Decimal64Array, Int32Array, StringArray, TimestampMicrosecondArray,
        TimestampNanosecondArray,
    };
    use arrow_buffer::{NullBuffer, OffsetBuffer};
    use connector_sdk::ErrorKind;

    use super::*;

    fn decimal(precision: u8, scale: i8) -> AnalyticsType {
        AnalyticsType::Decimal(AnalyticsDecimal::new(precision, scale).unwrap())
    }

    fn decimal128(values: Vec<Option<i128>>, precision: u8, scale: i8) -> ArrayRef {
        Arc::new(
            Decimal128Array::from(values)
                .with_precision_and_scale(precision, scale)
                .unwrap(),
        )
    }

    #[test]
    fn storage_types_pass_through() {
        let column: ArrayRef = Arc::new(Int32Array::from(vec![Some(1), None]));
        let converted = convert_column(&column, &AnalyticsType::Int).unwrap();
        assert!(Arc::ptr_eq(&column, &converted));
        let wide = decimal128(vec![Some(-1)], 38, 2);
        let converted = convert_column(&wide, &decimal(38, 2)).unwrap();
        assert!(Arc::ptr_eq(&wide, &converted));
    }

    #[test]
    fn decimals_narrow_to_the_storage_of_their_precision() {
        let narrow =
            convert_column(&decimal128(vec![Some(99_999), None], 5, 2), &decimal(5, 2)).unwrap();
        assert_eq!(
            narrow.as_ref(),
            &Decimal32Array::from(vec![Some(99_999), None])
                .with_precision_and_scale(5, 2)
                .unwrap()
        );
        let max18 = 999_999_999_999_999_999_i128;
        let medium =
            convert_column(&decimal128(vec![Some(-max18)], 18, 4), &decimal(18, 4)).unwrap();
        assert_eq!(
            medium.as_ref(),
            &Decimal64Array::from(vec![-999_999_999_999_999_999_i64])
                .with_precision_and_scale(18, 4)
                .unwrap()
        );
    }

    #[test]
    fn a_decimal_of_another_precision_is_checked_against_the_mapped_one() {
        // Databricks may widen the Arrow precision; the values must still fit.
        let wider = convert_column(&decimal128(vec![Some(12_345)], 38, 2), &decimal(5, 2)).unwrap();
        assert_eq!(wider.data_type(), &DataType::Decimal32(5, 2));
        let error =
            convert_column(&decimal128(vec![Some(100_000)], 38, 2), &decimal(5, 2)).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Data);
        assert!(!error.safe_message().contains("100000"));
    }

    #[test]
    fn a_decimal_of_another_scale_is_internal() {
        let error = convert_column(&decimal128(vec![Some(1)], 5, 3), &decimal(5, 2)).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Internal);
        assert!(error.safe_message().contains("Decimal128(5, 3)"), "{error}");
    }

    #[test]
    fn instants_are_labelled_utc_whatever_the_session_time_zone() {
        for zone in ["Etc/UTC", "UTC", "+00:00", "America/New_York"] {
            let column: ArrayRef =
                Arc::new(TimestampMicrosecondArray::from(vec![Some(1), None]).with_timezone(zone));
            let converted = convert_column(&column, &AnalyticsType::TimestampTz).unwrap();
            assert_eq!(
                converted.as_ref(),
                &TimestampMicrosecondArray::from(vec![Some(1), None]).with_timezone("UTC")
            );
        }
        let nanos: ArrayRef =
            Arc::new(TimestampNanosecondArray::from(vec![3_000]).with_timezone("Etc/UTC"));
        assert_eq!(
            convert_column(&nanos, &AnalyticsType::TimestampTz)
                .unwrap()
                .as_ref(),
            &TimestampMicrosecondArray::from(vec![3]).with_timezone("UTC")
        );
    }

    #[test]
    fn timestamps_with_and_without_a_zone_are_not_mixed() {
        let naive: ArrayRef = Arc::new(TimestampMicrosecondArray::from(vec![1]));
        let error = convert_column(&naive, &AnalyticsType::TimestampTz).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Internal);
        let zoned: ArrayRef =
            Arc::new(TimestampMicrosecondArray::from(vec![1]).with_timezone("UTC"));
        let error = convert_column(&zoned, &AnalyticsType::Timestamp).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Internal);
        let nanos: ArrayRef = Arc::new(TimestampNanosecondArray::from(vec![2_000]));
        assert_eq!(
            convert_column(&nanos, &AnalyticsType::Timestamp)
                .unwrap()
                .as_ref(),
            &TimestampMicrosecondArray::from(vec![2])
        );
    }

    #[test]
    fn arrays_are_rebuilt_with_converted_elements() {
        let element = Arc::new(Field::new("element", DataType::Decimal128(38, 2), true));
        let column: ArrayRef = Arc::new(ListArray::new(
            element,
            OffsetBuffer::from_lengths([2, 0, 1]),
            decimal128(vec![Some(150), None, Some(-1)], 38, 2),
            Some(NullBuffer::from(vec![true, false, true])),
        ));
        let target = AnalyticsType::Array(ArrayType::new(decimal(5, 2), true));
        let converted = convert_column(&column, &target).unwrap();
        assert_eq!(converted.data_type(), &target.storage_type());
        let lists = converted.as_any().downcast_ref::<ListArray>().unwrap();
        assert!(lists.is_null(1));
        assert_eq!(
            lists.value(0).as_ref(),
            &Decimal32Array::from(vec![Some(150), None])
                .with_precision_and_scale(5, 2)
                .unwrap()
        );
    }

    #[test]
    fn maps_are_rebuilt_with_converted_keys_and_values() {
        let entries = Arc::new(Field::new(
            "entries",
            DataType::Struct(
                vec![
                    Field::new("key", DataType::Utf8, false),
                    Field::new(
                        "value",
                        DataType::Timestamp(
                            analytics_types::arrow_schema::TimeUnit::Microsecond,
                            Some("Etc/UTC".into()),
                        ),
                        true,
                    ),
                ]
                .into(),
            ),
            false,
        ));
        let DataType::Struct(entry_fields) = entries.data_type().clone() else {
            unreachable!()
        };
        let column: ArrayRef = Arc::new(MapArray::new(
            entries,
            OffsetBuffer::from_lengths([2]),
            StructArray::new(
                entry_fields,
                vec![
                    Arc::new(StringArray::from(vec!["a", "b"])),
                    Arc::new(
                        TimestampMicrosecondArray::from(vec![Some(5), None])
                            .with_timezone("Etc/UTC"),
                    ),
                ],
                None,
            ),
            None,
            false,
        ));
        let target = AnalyticsType::Map(MapType::new(
            AnalyticsType::Text,
            AnalyticsType::TimestampTz,
            true,
        ));
        let converted = convert_column(&column, &target).unwrap();
        assert_eq!(converted.data_type(), &target.storage_type());
        let maps = converted.as_any().downcast_ref::<MapArray>().unwrap();
        assert_eq!(
            maps.values().as_ref(),
            &TimestampMicrosecondArray::from(vec![Some(5), None]).with_timezone("UTC")
        );
    }

    fn struct_target() -> AnalyticsType {
        AnalyticsType::Struct(StructType::new(vec![
            AnalyticsField::new("a", AnalyticsType::Int, true),
            AnalyticsField::new("b", decimal(9, 0), true),
        ]))
    }

    fn struct_column(names: [&str; 2]) -> ArrayRef {
        Arc::new(StructArray::new(
            vec![
                Field::new(names[0], DataType::Int32, true),
                Field::new(names[1], DataType::Decimal128(9, 0), true),
            ]
            .into(),
            vec![
                Arc::new(Int32Array::from(vec![Some(1), None])),
                decimal128(vec![Some(7), Some(8)], 9, 0),
            ],
            Some(NullBuffer::from(vec![true, false])),
        ))
    }

    #[test]
    fn structs_are_rebuilt_with_converted_fields() {
        let target = struct_target();
        let converted = convert_column(&struct_column(["a", "b"]), &target).unwrap();
        assert_eq!(converted.data_type(), &target.storage_type());
        let structs = converted.as_any().downcast_ref::<StructArray>().unwrap();
        assert!(structs.is_null(1));
        assert_eq!(structs.column(1).data_type(), &DataType::Decimal32(9, 0));
    }

    #[test]
    fn a_struct_with_other_fields_is_internal() {
        let error = convert_column(&struct_column(["b", "a"]), &struct_target()).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Internal);
    }

    #[test]
    fn a_value_that_cannot_be_carried_inside_a_nested_type_is_a_data_error() {
        let element = Arc::new(Field::new("element", DataType::Decimal128(38, 0), true));
        let column: ArrayRef = Arc::new(ListArray::new(
            element,
            OffsetBuffer::from_lengths([1]),
            decimal128(vec![Some(1_000)], 38, 0),
            None,
        ));
        let target = AnalyticsType::Array(ArrayType::new(decimal(3, 0), true));
        let error = convert_column(&column, &target).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Data);
    }

    #[test]
    fn other_forms_are_internal_and_name_the_arrived_type() {
        let text: ArrayRef = Arc::new(StringArray::from(vec!["1.5"]));
        let error = convert_column(&text, &decimal(5, 2)).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Internal);
        assert!(error.safe_message().contains("Utf8"), "{error}");
        assert!(!error.safe_message().contains("1.5"), "{error}");
    }
}
