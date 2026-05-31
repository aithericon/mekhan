use serde::{Deserialize, Serialize};

/// Comparison operator for a filter condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterOperator {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
    Contains,
    StartsWith,
    EndsWith,
    In,
    NotIn,
    IsNull,
    IsNotNull,
}

/// A typed filter value.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FilterValue {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    StringList(Vec<String>),
    Null,
}

impl FilterValue {
    /// Try to interpret this value as a string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            FilterValue::String(s) => Some(s),
            _ => None,
        }
    }
}

impl From<String> for FilterValue {
    fn from(s: String) -> Self {
        FilterValue::String(s)
    }
}

impl From<&str> for FilterValue {
    fn from(s: &str) -> Self {
        FilterValue::String(s.to_string())
    }
}

impl From<i64> for FilterValue {
    fn from(v: i64) -> Self {
        FilterValue::Int(v)
    }
}

impl From<bool> for FilterValue {
    fn from(v: bool) -> Self {
        FilterValue::Bool(v)
    }
}

/// A single filter condition: field + operator + value.
#[derive(Debug, Clone)]
pub struct FilterCondition {
    pub field: String,
    pub operator: FilterOperator,
    pub value: FilterValue,
}

/// A set of filter conditions (AND-combined).
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub conditions: Vec<FilterCondition>,
}

impl Filter {
    pub fn new(conditions: Vec<FilterCondition>) -> Self {
        Self { conditions }
    }

    pub fn single(
        field: impl Into<String>,
        operator: FilterOperator,
        value: impl Into<FilterValue>,
    ) -> Self {
        Self {
            conditions: vec![FilterCondition {
                field: field.into(),
                operator,
                value: value.into(),
            }],
        }
    }

    /// Builder: add another AND condition.
    pub fn and(
        mut self,
        field: impl Into<String>,
        operator: FilterOperator,
        value: impl Into<FilterValue>,
    ) -> Self {
        self.conditions.push(FilterCondition {
            field: field.into(),
            operator,
            value: value.into(),
        });
        self
    }

    pub fn is_empty(&self) -> bool {
        self.conditions.is_empty()
    }
}

/// Convert camelCase to snake_case.
pub fn camel_to_snake_case(field: &str) -> String {
    let mut result = String::with_capacity(field.len() + 4);
    for c in field.chars() {
        if c.is_uppercase() {
            if !result.is_empty() {
                result.push('_');
            }
            result.push(c.to_lowercase().next().unwrap());
        } else {
            result.push(c);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camel_to_snake() {
        assert_eq!(camel_to_snake_case("sourceNet"), "source_net");
        assert_eq!(camel_to_snake_case("createdAt"), "created_at");
        assert_eq!(camel_to_snake_case("name"), "name");
        assert_eq!(camel_to_snake_case("processId"), "process_id");
    }

    #[test]
    fn filter_builder() {
        let f = Filter::single("category", FilterOperator::Eq, "model").and(
            "source_net",
            FilterOperator::Contains,
            "surrogate",
        );
        assert_eq!(f.conditions.len(), 2);
    }
}
