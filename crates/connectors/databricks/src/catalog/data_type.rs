//! Databricks data types as `information_schema.columns.full_data_type`
//! spells them, such as `decimal(10,2)`, `string collate UTF8_LCASE`, or
//! `array<struct<a:int,`b c`:string>>`.

/// One Databricks data type, read from its `full_data_type` text.
///
/// The types the type mapping maps are distinguished; every other type keeps
/// its text as [`DataType::Other`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DataType {
    Boolean,
    TinyInt,
    SmallInt,
    Int,
    BigInt,
    Float,
    Double,
    Decimal {
        precision: u8,
        scale: u8,
    },
    /// `string`, with its collation when the text names one.
    String {
        collation: Option<String>,
    },
    Varchar {
        length: u32,
        collation: Option<String>,
    },
    Char {
        length: u32,
        collation: Option<String>,
    },
    Binary,
    Date,
    /// `timestamp`: an instant, shown in the session time zone.
    Timestamp,
    /// `timestamp_ntz`, whose fractional digits are 6 unless the text gives
    /// them.
    TimestampNtz {
        precision: u8,
    },
    /// `array<element>`.
    Array(Box<DataType>),
    /// `map<key,value>`.
    Map {
        key: Box<DataType>,
        value: Box<DataType>,
    },
    /// `struct<name:type,...>`.
    Struct(Vec<StructField>),
    /// Any other type, such as `interval day to second`, `variant`, or
    /// `geography(4326)`, as its text.
    Other(String),
}

/// One field of a `struct`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StructField {
    /// The name, without the backticks that quote it.
    pub(crate) name: String,
    pub(crate) data_type: DataType,
}

impl DataType {
    /// Reads `full_data_type`; `None` when the text is no type this grammar
    /// reads.
    ///
    /// Keywords are read without regard to case. A struct field is
    /// `name:type`, its name a run of letters, digits, and underscores, or
    /// quoted with backticks, with a backtick inside doubled. A `string`,
    /// `varchar(n)`, or `char(n)` may name a collation with `collate <name>`.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let mut parser = Parser { rest: text };
        let data_type = parser.data_type()?;
        parser.skip_whitespace();
        parser.rest.is_empty().then_some(data_type)
    }
}

/// A recursive-descent reader over the rest of a type's text.
struct Parser<'a> {
    rest: &'a str,
}

impl<'a> Parser<'a> {
    fn skip_whitespace(&mut self) {
        self.rest = self.rest.trim_start();
    }

    /// Consumes `token` after optional whitespace.
    fn eat(&mut self, token: char) -> bool {
        self.skip_whitespace();
        match self.rest.strip_prefix(token) {
            Some(rest) => {
                self.rest = rest;
                true
            }
            None => false,
        }
    }

    /// Consumes a run of letters, digits, and underscores after optional
    /// whitespace.
    fn word(&mut self) -> Option<&'a str> {
        self.skip_whitespace();
        let end = self
            .rest
            .find(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
            .unwrap_or(self.rest.len());
        if end == 0 {
            return None;
        }
        let (word, rest) = self.rest.split_at(end);
        self.rest = rest;
        Some(word)
    }

    /// Consumes `keyword`, in any case, when it is the next word.
    fn eat_keyword(&mut self, keyword: &str) -> bool {
        let saved = self.rest;
        if self
            .word()
            .is_some_and(|word| word.eq_ignore_ascii_case(keyword))
        {
            return true;
        }
        self.rest = saved;
        false
    }

    /// Consumes `(n, ...)`, the integer parameters of a type.
    fn parameters(&mut self) -> Option<Vec<u32>> {
        if !self.eat('(') {
            return Some(Vec::new());
        }
        let mut parameters = Vec::new();
        loop {
            parameters.push(self.word()?.parse().ok()?);
            if self.eat(')') {
                return Some(parameters);
            }
            if !self.eat(',') {
                return None;
            }
        }
    }

    /// Consumes `collate <name>`, when present; `None` when `collate` names
    /// nothing.
    fn collation(&mut self) -> Option<Option<String>> {
        if !self.eat_keyword("collate") {
            return Some(None);
        }
        self.skip_whitespace();
        let end = self
            .rest
            .find(|character: char| {
                !(character.is_ascii_alphanumeric() || character == '_' || character == '.')
            })
            .unwrap_or(self.rest.len());
        let (name, rest) = self.rest.split_at(end);
        self.rest = rest;
        (!name.is_empty()).then(|| Some(name.to_owned()))
    }

    fn data_type(&mut self) -> Option<DataType> {
        self.skip_whitespace();
        let start = self.rest;
        let name = self.word()?.to_ascii_lowercase();
        let data_type = match name.as_str() {
            "array" => {
                let element = self.angle(|parser| parser.data_type())?;
                DataType::Array(Box::new(element))
            }
            "map" => {
                let (key, value) = self.angle(|parser| {
                    let key = parser.data_type()?;
                    parser.eat(',').then_some(())?;
                    Some((key, parser.data_type()?))
                })?;
                DataType::Map {
                    key: Box::new(key),
                    value: Box::new(value),
                }
            }
            "struct" => DataType::Struct(self.angle(|parser| parser.struct_fields())?),
            "boolean" => self.no_parameters(DataType::Boolean)?,
            "tinyint" => self.no_parameters(DataType::TinyInt)?,
            "smallint" => self.no_parameters(DataType::SmallInt)?,
            "int" => self.no_parameters(DataType::Int)?,
            "bigint" => self.no_parameters(DataType::BigInt)?,
            "float" => self.no_parameters(DataType::Float)?,
            "double" => self.no_parameters(DataType::Double)?,
            "binary" => self.no_parameters(DataType::Binary)?,
            "date" => self.no_parameters(DataType::Date)?,
            "timestamp" => self.no_parameters(DataType::Timestamp)?,
            "decimal" => match self.parameters()?.as_slice() {
                [precision, scale] => DataType::Decimal {
                    precision: u8::try_from(*precision).ok()?,
                    scale: u8::try_from(*scale).ok()?,
                },
                _ => return None,
            },
            "timestamp_ntz" => match self.parameters()?.as_slice() {
                [] => DataType::TimestampNtz { precision: 6 },
                [precision] => DataType::TimestampNtz {
                    precision: u8::try_from(*precision).ok()?,
                },
                _ => return None,
            },
            "string" => {
                if !self.parameters()?.is_empty() {
                    return None;
                }
                DataType::String {
                    collation: self.collation()?,
                }
            }
            "varchar" | "char" => {
                let [length] = self.parameters()?[..] else {
                    return None;
                };
                let collation = self.collation()?;
                if name == "varchar" {
                    DataType::Varchar { length, collation }
                } else {
                    DataType::Char { length, collation }
                }
            }
            _ => {
                self.rest = start;
                DataType::Other(self.other()?)
            }
        };
        Some(data_type)
    }

    /// Returns `data_type` after checking that no parameters follow.
    fn no_parameters(&mut self, data_type: DataType) -> Option<DataType> {
        self.parameters()?.is_empty().then_some(data_type)
    }

    /// Reads `<...>` around what `inner` reads.
    fn angle<T>(&mut self, inner: impl FnOnce(&mut Self) -> Option<T>) -> Option<T> {
        self.eat('<').then_some(())?;
        let value = inner(self)?;
        self.eat('>').then_some(value)
    }

    /// Reads `name:type, ...` up to the closing `>`; a struct may have no
    /// fields.
    fn struct_fields(&mut self) -> Option<Vec<StructField>> {
        let mut fields = Vec::new();
        self.skip_whitespace();
        if self.rest.starts_with('>') {
            return Some(fields);
        }
        loop {
            let name = self.field_name()?;
            self.eat(':').then_some(())?;
            fields.push(StructField {
                name,
                data_type: self.data_type()?,
            });
            if !self.eat(',') {
                return Some(fields);
            }
        }
    }

    /// Reads a field name, bare or quoted with backticks.
    fn field_name(&mut self) -> Option<String> {
        self.skip_whitespace();
        let Some(quoted) = self.rest.strip_prefix('`') else {
            return self.word().map(str::to_owned);
        };
        let mut name = String::new();
        let mut characters = quoted.char_indices();
        loop {
            let (index, character) = characters.next()?;
            if character != '`' {
                name.push(character);
                continue;
            }
            if quoted[index + 1..].starts_with('`') {
                characters.next();
                name.push('`');
                continue;
            }
            self.rest = &quoted[index + 1..];
            return Some(name);
        }
    }

    /// Reads the text of a type this grammar does not distinguish, up to the
    /// `,` or `>` that ends it at its own nesting level.
    fn other(&mut self) -> Option<String> {
        let mut depth = 0usize;
        let end = self
            .rest
            .char_indices()
            .find(|(_, character)| match character {
                '(' | '<' => {
                    depth += 1;
                    false
                }
                ')' | '>' if depth > 0 => {
                    depth -= 1;
                    false
                }
                ',' | '>' => depth == 0,
                _ => false,
            })
            .map_or(self.rest.len(), |(index, _)| index);
        let (text, rest) = self.rest.split_at(end);
        self.rest = rest;
        let text = text.trim_end();
        (depth == 0 && !text.is_empty()).then(|| text.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> DataType {
        DataType::parse(text).unwrap_or_else(|| panic!("`{text}` is a type"))
    }

    fn field(name: &str, data_type: DataType) -> StructField {
        StructField {
            name: name.to_owned(),
            data_type,
        }
    }

    #[test]
    fn primitive_types_are_read_in_any_case() {
        for (text, expected) in [
            ("boolean", DataType::Boolean),
            ("tinyint", DataType::TinyInt),
            ("smallint", DataType::SmallInt),
            ("int", DataType::Int),
            ("BIGINT", DataType::BigInt),
            ("float", DataType::Float),
            ("double", DataType::Double),
            ("binary", DataType::Binary),
            ("date", DataType::Date),
            ("timestamp", DataType::Timestamp),
            ("timestamp_ntz", DataType::TimestampNtz { precision: 6 }),
            ("timestamp_ntz(9)", DataType::TimestampNtz { precision: 9 }),
            (
                "Decimal(38, 18)",
                DataType::Decimal {
                    precision: 38,
                    scale: 18,
                },
            ),
            ("string", DataType::String { collation: None }),
            (
                "varchar(10)",
                DataType::Varchar {
                    length: 10,
                    collation: None,
                },
            ),
            (
                "char(3)",
                DataType::Char {
                    length: 3,
                    collation: None,
                },
            ),
        ] {
            assert_eq!(parse(text), expected, "{text}");
        }
    }

    #[test]
    fn collations_are_read_after_character_types() {
        assert_eq!(
            parse("string collate UTF8_LCASE"),
            DataType::String {
                collation: Some("UTF8_LCASE".to_owned())
            }
        );
        assert_eq!(
            parse("STRING COLLATE UNICODE_CI_AI"),
            DataType::String {
                collation: Some("UNICODE_CI_AI".to_owned())
            }
        );
        assert_eq!(
            parse("varchar(5) collate system.builtin.UTF8_BINARY"),
            DataType::Varchar {
                length: 5,
                collation: Some("system.builtin.UTF8_BINARY".to_owned())
            }
        );
        assert_eq!(
            parse("char(2) collate UTF8_BINARY_RTRIM"),
            DataType::Char {
                length: 2,
                collation: Some("UTF8_BINARY_RTRIM".to_owned())
            }
        );
        assert_eq!(
            parse("array<string collate UTF8_LCASE>"),
            DataType::Array(Box::new(DataType::String {
                collation: Some("UTF8_LCASE".to_owned())
            }))
        );
    }

    #[test]
    fn nested_types_are_read_recursively() {
        assert_eq!(
            parse("array<map<string,array<decimal(5,2)>>>"),
            DataType::Array(Box::new(DataType::Map {
                key: Box::new(DataType::String { collation: None }),
                value: Box::new(DataType::Array(Box::new(DataType::Decimal {
                    precision: 5,
                    scale: 2
                }))),
            }))
        );
        assert_eq!(
            parse("struct<a:int,b:struct<c:timestamp_ntz,d:array<binary>>>"),
            DataType::Struct(vec![
                field("a", DataType::Int),
                field(
                    "b",
                    DataType::Struct(vec![
                        field("c", DataType::TimestampNtz { precision: 6 }),
                        field("d", DataType::Array(Box::new(DataType::Binary))),
                    ])
                ),
            ])
        );
        assert_eq!(
            parse("map < int , struct < x : double > >"),
            DataType::Map {
                key: Box::new(DataType::Int),
                value: Box::new(DataType::Struct(vec![field("x", DataType::Double)])),
            }
        );
        assert_eq!(parse("struct<>"), DataType::Struct(Vec::new()));
    }

    #[test]
    fn backticks_quote_field_names() {
        assert_eq!(
            parse("struct<`a b`:int,`x``y`:string,`c:d,e>`:boolean,``:date>"),
            DataType::Struct(vec![
                field("a b", DataType::Int),
                field("x`y", DataType::String { collation: None }),
                field("c:d,e>", DataType::Boolean),
                field("", DataType::Date),
            ])
        );
    }

    #[test]
    fn other_types_keep_their_text() {
        for text in [
            "interval year to month",
            "interval day to second",
            "interval day",
            "variant",
            "void",
            "geography(4326)",
            "geometry(OGC:CRS84)",
            "object",
        ] {
            assert_eq!(parse(text), DataType::Other(text.to_owned()), "{text}");
        }
        assert_eq!(
            parse("struct<a:interval day to second,v:variant>"),
            DataType::Struct(vec![
                field("a", DataType::Other("interval day to second".to_owned())),
                field("v", DataType::Other("variant".to_owned())),
            ])
        );
        assert_eq!(
            parse("map<string,geography(4326)>"),
            DataType::Map {
                key: Box::new(DataType::String { collation: None }),
                value: Box::new(DataType::Other("geography(4326)".to_owned())),
            }
        );
    }

    #[test]
    fn malformed_text_is_no_type() {
        for text in [
            "",
            "array<int",
            "array<>",
            "map<int>",
            "map<int,string,int>",
            "struct<a int>",
            "struct<a:int,>",
            "struct<`a:int>",
            "string collate",
            "string collate ,",
            "decimal",
            "decimal(10)",
            "decimal(300,2)",
            "decimal(10,x)",
            "varchar",
            "char(1,2)",
            "string(5)",
            "int(4)",
            "timestamp_ntz(6,1)",
            "int extra",
            "array<int>>",
            "geography(4326",
        ] {
            assert_eq!(DataType::parse(text), None, "{text}");
        }
    }
}
