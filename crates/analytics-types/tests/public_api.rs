use analytics_types::{
    AnalyticsField, AnalyticsSchema, AnalyticsType, ArrayType, DecimalType, EnumType, MapType,
    StructType, TypeError,
    arrow_schema::{
        DataType, Field, Schema, TimeUnit,
        extension::{EXTENSION_TYPE_METADATA_KEY, EXTENSION_TYPE_NAME_KEY},
    },
};

#[test]
fn public_schema_api_round_trips_every_type_parameter_and_nested_field() {
    let nested = AnalyticsType::Struct(StructType::new(vec![
        AnalyticsField::new(
            "amount",
            AnalyticsType::Decimal(DecimalType::new(38, 18).unwrap()),
            false,
        ),
        AnalyticsField::new(
            "status",
            AnalyticsType::Enum(EnumType::new(vec!["new".to_owned(), "done".to_owned()])),
            true,
        ),
        AnalyticsField::new(
            "timestamps",
            AnalyticsType::Array(ArrayType::new(AnalyticsType::TimestampTz, true)),
            false,
        ),
        AnalyticsField::new(
            "attributes",
            AnalyticsType::Map(MapType::new(AnalyticsType::Text, AnalyticsType::Json, true)),
            true,
        ),
    ]));

    let schema = AnalyticsSchema::new(vec![
        AnalyticsField::new("id", AnalyticsType::Uuid, false),
        AnalyticsField::new("payload", nested, true),
        AnalyticsField::new("year", AnalyticsType::Year, false),
        AnalyticsField::new("local_time", AnalyticsType::TimeWithTimeZone, true),
    ]);

    let arrow = schema.to_arrow();
    assert_eq!(
        arrow.field(0).metadata().get(EXTENSION_TYPE_NAME_KEY),
        Some(&"scalardb.analytics.uuid".to_owned())
    );
    assert_eq!(arrow.field(0).data_type(), &DataType::Utf8);
    assert_eq!(AnalyticsSchema::try_from_arrow(&arrow).unwrap(), schema);
    assert_eq!(
        AnalyticsSchema::try_from_arrow(&arrow).unwrap().to_arrow(),
        arrow
    );
}

#[test]
fn decimal_boundaries_are_validated() {
    assert_eq!(DecimalType::new(1, 0).unwrap().precision(), 1);
    assert_eq!(DecimalType::new(38, 38).unwrap().scale(), 38);
    assert_eq!(DecimalType::new(2, -3).unwrap().scale(), -3);
    assert_eq!(
        DecimalType::new(0, 0),
        Err(TypeError::InvalidDecimal {
            precision: 0,
            scale: 0
        })
    );
    assert!(DecimalType::new(39, 0).is_err());
    assert!(DecimalType::new(10, 11).is_err());
}

#[test]
fn invalid_extension_encodings_are_typed_errors() {
    let unknown = Field::new("value", DataType::Utf8, false).with_metadata(
        [
            (
                EXTENSION_TYPE_NAME_KEY.to_owned(),
                "scalardb.analytics.money".to_owned(),
            ),
            (EXTENSION_TYPE_METADATA_KEY.to_owned(), "{}".to_owned()),
        ]
        .into_iter()
        .collect(),
    );
    assert!(matches!(
        AnalyticsField::try_from_arrow(&unknown),
        Err(TypeError::UnknownExtensionName { .. })
    ));

    let invalid_enum = Field::new("value", DataType::Utf8, false).with_metadata(
        [
            (
                EXTENSION_TYPE_NAME_KEY.to_owned(),
                "scalardb.analytics.enum".to_owned(),
            ),
            (
                EXTENSION_TYPE_METADATA_KEY.to_owned(),
                r#"{"members":"not-an-array"}"#.to_owned(),
            ),
        ]
        .into_iter()
        .collect(),
    );
    assert!(matches!(
        AnalyticsField::try_from_arrow(&invalid_enum),
        Err(TypeError::InvalidExtensionMetadata { .. })
    ));
}

#[test]
fn extension_metadata_spelling_does_not_change_analytics_equality() {
    let arrow = Field::new("status", DataType::Utf8, true).with_metadata(
        [
            (
                EXTENSION_TYPE_NAME_KEY.to_owned(),
                "scalardb.analytics.enum".to_owned(),
            ),
            (
                EXTENSION_TYPE_METADATA_KEY.to_owned(),
                r#"{ "members": [] }"#.to_owned(),
            ),
        ]
        .into_iter()
        .collect(),
    );

    let parsed = AnalyticsField::try_from_arrow(&arrow).unwrap();
    let constructed = AnalyticsField::new(
        "status",
        AnalyticsType::Enum(EnumType::new(Vec::new())),
        true,
    );
    assert_eq!(parsed, constructed);
    assert_eq!(
        parsed
            .to_arrow()
            .metadata()
            .get(EXTENSION_TYPE_METADATA_KEY)
            .map(String::as_str),
        Some(r#"{"members":[]}"#)
    );

    let escaped = Field::new("status", DataType::Utf8, false).with_metadata(
        [
            (
                EXTENSION_TYPE_NAME_KEY.to_owned(),
                "scalardb.analytics.enum".to_owned(),
            ),
            (
                EXTENSION_TYPE_METADATA_KEY.to_owned(),
                r#"{"members":["\u0061"]}"#.to_owned(),
            ),
        ]
        .into_iter()
        .collect(),
    );
    assert_eq!(
        AnalyticsField::try_from_arrow(&escaped)
            .unwrap()
            .data_type(),
        &AnalyticsType::Enum(EnumType::new(vec!["a".to_owned()]))
    );
}

#[test]
fn unsupported_arrow_parameters_are_typed_errors() {
    let non_utc = Schema::new(vec![Field::new(
        "created_at",
        DataType::Timestamp(TimeUnit::Microsecond, Some("Asia/Tokyo".into())),
        false,
    )]);
    assert!(matches!(
        AnalyticsSchema::try_from_arrow(&non_utc),
        Err(TypeError::UnsupportedArrowType { .. })
    ));

    let unsigned = Schema::new(vec![Field::new("value", DataType::UInt64, false)]);
    assert!(matches!(
        AnalyticsSchema::try_from_arrow(&unsigned),
        Err(TypeError::UnsupportedArrowType { .. })
    ));
}

#[test]
fn every_direct_arrow_scalar_type_round_trips() {
    let analytics_types = vec![
        AnalyticsType::Boolean,
        AnalyticsType::Byte,
        AnalyticsType::SmallInt,
        AnalyticsType::Int,
        AnalyticsType::BigInt,
        AnalyticsType::Float,
        AnalyticsType::Double,
        AnalyticsType::Decimal(DecimalType::new(2, -3).unwrap()),
        AnalyticsType::Text,
        AnalyticsType::Blob,
        AnalyticsType::Date,
        AnalyticsType::Time,
        AnalyticsType::Timestamp,
        AnalyticsType::TimestampTz,
        AnalyticsType::Duration,
        AnalyticsType::Interval,
    ];
    let schema = AnalyticsSchema::new(
        analytics_types
            .into_iter()
            .enumerate()
            .map(|(index, data_type)| {
                AnalyticsField::new(format!("field_{index}"), data_type, index % 2 == 0)
            })
            .collect(),
    );

    assert_eq!(
        AnalyticsSchema::try_from_arrow(&schema.to_arrow()).unwrap(),
        schema
    );
}

#[test]
fn malformed_extension_encodings_are_typed_errors() {
    let wrong_storage = Field::new("id", DataType::Int64, false).with_metadata(
        [
            (
                EXTENSION_TYPE_NAME_KEY.to_owned(),
                "scalardb.analytics.uuid".to_owned(),
            ),
            (EXTENSION_TYPE_METADATA_KEY.to_owned(), "{}".to_owned()),
        ]
        .into_iter()
        .collect(),
    );
    assert!(matches!(
        AnalyticsField::try_from_arrow(&wrong_storage),
        Err(TypeError::ExtensionStorageMismatch { .. })
    ));

    let orphan_metadata = Field::new("id", DataType::Utf8, false).with_metadata(
        [(EXTENSION_TYPE_METADATA_KEY.to_owned(), "{}".to_owned())]
            .into_iter()
            .collect(),
    );
    assert_eq!(
        AnalyticsField::try_from_arrow(&orphan_metadata),
        Err(TypeError::OrphanExtensionMetadata)
    );
}
