use serde::{Deserialize, Serialize};

/// Cross-format data type classification.
///
/// Aligned with Arrow's type system vocabulary but independent of the `arrow` crate.
/// Conversion to/from Arrow types can be added behind a feature flag.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataType {
    Boolean,
    Int8,
    Int16,
    Int32,
    Int64,
    UInt8,
    UInt16,
    UInt32,
    UInt64,
    Float32,
    Float64,
    /// UTF-8 string.
    String,
    /// Variable-length binary.
    Binary,
    /// Timestamp with optional timezone.
    Timestamp {
        timezone: Option<std::string::String>,
    },
    /// Calendar date (no time component).
    Date,
    /// Time of day (no date component).
    Time,
    /// Duration / interval.
    Duration,
    /// Ordered list of a single element type.
    List(Box<DataType>),
    /// Named fields (e.g., Parquet group, Arrow struct).
    Struct(Vec<(std::string::String, DataType)>),
    /// Dictionary-encoded (index type + value type).
    Dictionary {
        index: Box<DataType>,
        value: Box<DataType>,
    },
    /// Format reported a type we cannot classify.
    Unknown(std::string::String),
}

impl DataType {
    /// Returns `true` if this type represents a numeric value.
    pub fn is_numeric(&self) -> bool {
        matches!(
            self,
            DataType::Int8
                | DataType::Int16
                | DataType::Int32
                | DataType::Int64
                | DataType::UInt8
                | DataType::UInt16
                | DataType::UInt32
                | DataType::UInt64
                | DataType::Float32
                | DataType::Float64
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_simple_types() {
        for dt in [
            DataType::Boolean,
            DataType::Int64,
            DataType::Float64,
            DataType::String,
            DataType::Binary,
            DataType::Date,
            DataType::Time,
            DataType::Duration,
        ] {
            let json = serde_json::to_string(&dt).unwrap();
            let back: DataType = serde_json::from_str(&json).unwrap();
            assert_eq!(dt, back);
        }
    }

    #[test]
    fn round_trip_complex_types() {
        let ts = DataType::Timestamp {
            timezone: Some("UTC".into()),
        };
        let list = DataType::List(Box::new(DataType::Int32));
        let strct = DataType::Struct(vec![
            ("name".into(), DataType::String),
            ("age".into(), DataType::Int32),
        ]);
        let dict = DataType::Dictionary {
            index: Box::new(DataType::UInt32),
            value: Box::new(DataType::String),
        };
        let unknown = DataType::Unknown("custom_type".into());

        for dt in [ts, list, strct, dict, unknown] {
            let json = serde_json::to_string(&dt).unwrap();
            let back: DataType = serde_json::from_str(&json).unwrap();
            assert_eq!(dt, back);
        }
    }
}
