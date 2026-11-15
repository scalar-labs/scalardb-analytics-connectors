//! The MySQL type mapping (`crates/connectors/mysql/README.md#type-mapping`).

use analytics_types::{AnalyticsType, DecimalType, EnumType};
use connector_sdk::UnsupportedReason;

use super::MysqlColumn;
use crate::MysqlSettings;

/// Applies the type mapping to one column of a Data Source with `settings`.
///
/// The rule is looked up by `DATA_TYPE`. `COLUMN_TYPE` adds what the name
/// leaves out: `unsigned`, a display width, a decimal's precision and scale,
/// and an enum's members. `tinyint(1)`, which MySQL's `BOOL` and `BOOLEAN`
/// declare, is `Boolean` with `tinyint1_as_boolean` and otherwise an integer
/// like any other `tinyint`; `bit(1)` holds only 0 and 1 and is `Boolean`.
pub(crate) fn map_type(
    column: &MysqlColumn,
    settings: &MysqlSettings,
) -> Result<AnalyticsType, UnsupportedReason> {
    let column_type = column.data_source_type.as_str();
    let unsigned = column_type
        .split(' ')
        .any(|attribute| attribute == "unsigned");
    let mapped = match (column.data_type.as_str(), unsigned) {
        ("tinyint", false) if settings.tinyint1_as_boolean && column_type == "tinyint(1)" => {
            AnalyticsType::Boolean
        }
        ("bit", _) if column_type == "bit(1)" => AnalyticsType::Boolean,
        ("tinyint", false) => AnalyticsType::Byte,
        ("smallint", false) => AnalyticsType::SmallInt,
        ("mediumint" | "int", false) => AnalyticsType::Int,
        ("bigint", false) => AnalyticsType::BigInt,
        ("tinyint", true) => AnalyticsType::UnsignedByte,
        ("smallint", true) => AnalyticsType::UnsignedSmallInt,
        ("mediumint" | "int", true) => AnalyticsType::UnsignedInt,
        ("bigint", true) => AnalyticsType::UnsignedBigInt,
        ("float", _) => AnalyticsType::Float,
        ("double", _) => AnalyticsType::Double,
        ("decimal", _) => return decimal(&column.data_source_type),
        ("char" | "varchar" | "tinytext" | "text" | "mediumtext" | "longtext", _) => {
            AnalyticsType::Text
        }
        ("binary" | "varbinary" | "tinyblob" | "blob" | "mediumblob" | "longblob", _) => {
            AnalyticsType::Blob
        }
        ("date", _) => AnalyticsType::Date,
        ("datetime", _) => AnalyticsType::Timestamp,
        ("timestamp", _) => AnalyticsType::TimestampTz,
        ("year", _) => AnalyticsType::Year,
        ("enum", _) => {
            let members =
                enum_members(&column.data_source_type).ok_or(UnsupportedReason::NotMapped)?;
            AnalyticsType::Enum(EnumType::new(members))
        }
        ("json", _) => AnalyticsType::Json,
        _ => return Err(UnsupportedReason::NotMapped),
    };
    Ok(mapped)
}

/// Maps `decimal(p,s)`, optionally `unsigned`, to `Decimal(p, s)`; MySQL's
/// precision, at most 65, is within the Analytics decimal's.
fn decimal(column_type: &str) -> Result<AnalyticsType, UnsupportedReason> {
    let parameters = column_type
        .strip_prefix("decimal(")
        .and_then(|rest| rest.split_once(')'))
        .and_then(|(parameters, _)| parameters.split_once(','));
    let Some((precision, scale)) = parameters else {
        return Err(UnsupportedReason::NotMapped);
    };
    let (Ok(precision), Ok(scale)) = (precision.parse::<u8>(), scale.parse::<i8>()) else {
        return Err(UnsupportedReason::ParameterOutOfRange);
    };
    DecimalType::new(precision, scale)
        .map(AnalyticsType::Decimal)
        .map_err(|_| UnsupportedReason::ParameterOutOfRange)
}

/// Reads an enum's members in declaration order from its `COLUMN_TYPE`, such
/// as `enum('a','it''s')`.
///
/// Each member is quoted, with a quote inside doubled. MySQL 8.4 and 9.7 also
/// escape a backslash as `\\`, a newline as `\n`, a carriage return as `\r`,
/// and NUL as `\0`, and print every other character as it is.
fn enum_members(column_type: &str) -> Option<Vec<String>> {
    let mut rest = column_type.strip_prefix("enum(")?.strip_suffix(')')?;
    let mut members = Vec::new();
    loop {
        let mut chars = rest.strip_prefix('\'')?.char_indices();
        let mut member = String::new();
        let end = loop {
            let (index, character) = chars.next()?;
            match character {
                '\\' => member.push(match chars.next()?.1 {
                    '\\' => '\\',
                    'n' => '\n',
                    'r' => '\r',
                    '0' => '\0',
                    _ => return None,
                }),
                '\'' => match chars.clone().next() {
                    Some((_, '\'')) => {
                        chars.next();
                        member.push('\'');
                    }
                    _ => break index + 1,
                },
                _ => member.push(character),
            }
        };
        members.push(member);
        rest = &rest[1 + end..];
        match rest.strip_prefix(',') {
            Some(next) => rest = next,
            None if rest.is_empty() => return Some(members),
            None => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT: MysqlSettings = MysqlSettings {
        tinyint1_as_boolean: false,
    };
    const TINYINT1_AS_BOOLEAN: MysqlSettings = MysqlSettings {
        tinyint1_as_boolean: true,
    };

    fn column(data_type: &str, data_source_type: &str) -> MysqlColumn {
        MysqlColumn {
            name: "c".to_owned(),
            ordinal_position: 1,
            nullable: true,
            data_source_type: data_source_type.to_owned(),
            data_type: data_type.to_owned(),
            character_set: None,
            collation: None,
            comment: None,
            value_source: None,
        }
    }

    #[test]
    fn integers_keep_their_signedness_and_width() {
        for (data_type, column_type, expected) in [
            ("tinyint", "tinyint", AnalyticsType::Byte),
            ("tinyint", "tinyint(1)", AnalyticsType::Byte),
            ("mediumint", "mediumint", AnalyticsType::Int),
            ("tinyint", "tinyint unsigned", AnalyticsType::UnsignedByte),
            (
                "smallint",
                "smallint unsigned",
                AnalyticsType::UnsignedSmallInt,
            ),
            (
                "mediumint",
                "mediumint unsigned",
                AnalyticsType::UnsignedInt,
            ),
            ("int", "int unsigned zerofill", AnalyticsType::UnsignedInt),
            ("bigint", "bigint unsigned", AnalyticsType::UnsignedBigInt),
        ] {
            assert_eq!(
                map_type(&column(data_type, column_type), &DEFAULT),
                Ok(expected),
                "{column_type}"
            );
        }
    }

    #[test]
    fn tinyint1_is_boolean_only_by_the_option_and_bit1_always() {
        let tinyint1 = column("tinyint", "tinyint(1)");
        assert_eq!(map_type(&tinyint1, &DEFAULT), Ok(AnalyticsType::Byte));
        assert_eq!(
            map_type(&tinyint1, &TINYINT1_AS_BOOLEAN),
            Ok(AnalyticsType::Boolean)
        );
        for other in [
            column("tinyint", "tinyint"),
            column("tinyint", "tinyint(1) unsigned"),
        ] {
            assert_ne!(
                map_type(&other, &TINYINT1_AS_BOOLEAN),
                Ok(AnalyticsType::Boolean)
            );
        }
        assert_eq!(
            map_type(&column("bit", "bit(1)"), &DEFAULT),
            Ok(AnalyticsType::Boolean)
        );
    }

    #[test]
    fn decimals_map_within_the_decimal_range() {
        assert_eq!(
            map_type(&column("decimal", "decimal(10,2) unsigned"), &DEFAULT),
            Ok(AnalyticsType::Decimal(DecimalType::new(10, 2).unwrap()))
        );
        assert_eq!(
            map_type(&column("decimal", "decimal(38,0)"), &DEFAULT),
            Ok(AnalyticsType::Decimal(DecimalType::new(38, 0).unwrap()))
        );
        assert_eq!(
            map_type(&column("decimal", "decimal(65,30)"), &DEFAULT),
            Ok(AnalyticsType::Decimal(DecimalType::new(65, 30).unwrap()))
        );
    }

    #[test]
    fn enum_members_keep_their_order_and_quotes() {
        assert_eq!(
            map_type(&column("enum", "enum('sad','it''s','a,b','')"), &DEFAULT),
            Ok(AnalyticsType::Enum(EnumType::new(vec![
                "sad".to_owned(),
                "it's".to_owned(),
                "a,b".to_owned(),
                String::new(),
            ])))
        );
        assert_eq!(enum_members("enum('a'"), None);
        assert_eq!(enum_members("enum('a' 'b')"), None);
    }

    #[test]
    fn enum_members_unescape_what_column_type_escapes() {
        // `COLUMN_TYPE` of `enum('a\\b', 'n\nl', 'z\0z', 'c\rr', 't\tz', 'k\\')`
        // on MySQL 8.4 and 9.7.
        let column_type = "enum('a\\\\b','n\\nl','z\\0z','c\\rr','t\tz','k\\\\')";
        assert_eq!(
            enum_members(column_type),
            Some(
                ["a\\b", "n\nl", "z\0z", "c\rr", "t\tz", "k\\"]
                    .map(str::to_owned)
                    .to_vec()
            )
        );
        assert_eq!(enum_members("enum('a\\qb')"), None);
    }

    #[test]
    fn types_outside_the_mapping_are_not_mapped() {
        for (data_type, column_type) in [
            ("time", "time"),
            ("bit", "bit(3)"),
            ("set", "set('a','b')"),
            ("geometry", "geometry"),
        ] {
            assert_eq!(
                map_type(&column(data_type, column_type), &DEFAULT),
                Err(UnsupportedReason::NotMapped)
            );
        }
    }
}
