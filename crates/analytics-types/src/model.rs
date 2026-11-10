//! The source-independent Analytics schema model.

use crate::TypeError;

/// A source-independent federation schema.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AnalyticsSchema {
    fields: Vec<AnalyticsField>,
}

impl AnalyticsSchema {
    pub fn new(fields: Vec<AnalyticsField>) -> Self {
        Self { fields }
    }

    pub fn fields(&self) -> &[AnalyticsField] {
        &self.fields
    }
}

/// A named, nullable Analytics type at a schema or struct position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnalyticsField {
    pub(crate) name: String,
    pub(crate) data_type: AnalyticsType,
    pub(crate) nullable: bool,
}

impl AnalyticsField {
    pub fn new(name: impl Into<String>, data_type: AnalyticsType, nullable: bool) -> Self {
        Self {
            name: name.into(),
            data_type,
            nullable,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn data_type(&self) -> &AnalyticsType {
        &self.data_type
    }

    pub fn is_nullable(&self) -> bool {
        self.nullable
    }
}

/// A type in the source-independent Analytics federation schema.
///
/// Variants are peers in one semantic type system. Their source, Arrow, and
/// Query Engine representations are owned by mappings at those boundaries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnalyticsType {
    Boolean,
    Byte,
    SmallInt,
    Int,
    BigInt,
    Float,
    Double,
    Decimal(DecimalType),
    Text,
    Blob,
    Date,
    Time,
    Timestamp,
    TimestampTz,
    Duration,
    Interval,
    Array(ArrayType),
    Map(MapType),
    Struct(StructType),
    Uuid,
    Json,
    Enum(EnumType),
    Year,
    TimeWithTimeZone,
}

/// A validated decimal type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecimalType {
    precision: u8,
    scale: i8,
}

impl DecimalType {
    /// Creates a decimal with Arrow-compatible precision and scale parameters.
    pub fn new(precision: u8, scale: i8) -> Result<Self, TypeError> {
        if !(1..=38).contains(&precision) || (scale > 0 && scale as u8 > precision) {
            return Err(TypeError::InvalidDecimal { precision, scale });
        }
        Ok(Self { precision, scale })
    }

    pub fn precision(self) -> u8 {
        self.precision
    }

    pub fn scale(self) -> i8 {
        self.scale
    }
}

/// An Analytics array element type and its nullability.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArrayType {
    pub(crate) element_type: Box<AnalyticsType>,
    pub(crate) element_nullable: bool,
}

impl ArrayType {
    pub fn new(element_type: AnalyticsType, element_nullable: bool) -> Self {
        Self {
            element_type: Box::new(element_type),
            element_nullable,
        }
    }

    pub fn element_type(&self) -> &AnalyticsType {
        &self.element_type
    }

    pub fn is_element_nullable(&self) -> bool {
        self.element_nullable
    }
}

/// An Analytics map key and value type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapType {
    pub(crate) key_type: Box<AnalyticsType>,
    pub(crate) value_type: Box<AnalyticsType>,
    pub(crate) value_nullable: bool,
}

impl MapType {
    /// Creates a map. Map keys are always non-nullable.
    pub fn new(key_type: AnalyticsType, value_type: AnalyticsType, value_nullable: bool) -> Self {
        Self {
            key_type: Box::new(key_type),
            value_type: Box::new(value_type),
            value_nullable,
        }
    }

    pub fn key_type(&self) -> &AnalyticsType {
        &self.key_type
    }

    pub fn value_type(&self) -> &AnalyticsType {
        &self.value_type
    }

    pub fn is_value_nullable(&self) -> bool {
        self.value_nullable
    }
}

/// An ordered set of named Analytics fields.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StructType {
    pub(crate) fields: Vec<AnalyticsField>,
}

impl StructType {
    pub fn new(fields: Vec<AnalyticsField>) -> Self {
        Self { fields }
    }

    pub fn fields(&self) -> &[AnalyticsField] {
        &self.fields
    }
}

/// A sequence of enum member labels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnumType {
    members: Vec<String>,
}

impl EnumType {
    pub fn new(members: Vec<String>) -> Self {
        Self { members }
    }

    pub fn members(&self) -> &[String] {
        &self.members
    }
}
