use serde::{Deserialize, Serialize};

/// Page-based pagination parameters.
#[derive(Debug, Clone, Deserialize)]
pub struct PageQuery {
    #[serde(default)]
    pub page: i64,
    #[serde(default = "default_page_size")]
    pub page_size: i64,
}

fn default_page_size() -> i64 {
    20
}

impl Default for PageQuery {
    fn default() -> Self {
        Self {
            page: 0,
            page_size: default_page_size(),
        }
    }
}

impl PageQuery {
    pub fn offset(&self) -> i64 {
        self.page * self.page_size
    }

    pub fn limit(&self) -> i64 {
        self.page_size.min(500)
    }
}

/// Sort direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortDirection {
    Asc,
    Desc,
}

/// Sort specification: field + direction.
#[derive(Debug, Clone)]
pub struct Sort {
    pub field: String,
    pub direction: SortDirection,
}

impl Sort {
    pub fn new(field: impl Into<String>, direction: SortDirection) -> Self {
        Self {
            field: field.into(),
            direction,
        }
    }

    pub fn asc(field: impl Into<String>) -> Self {
        Self::new(field, SortDirection::Asc)
    }

    pub fn desc(field: impl Into<String>) -> Self {
        Self::new(field, SortDirection::Desc)
    }

    /// Parse from query string format: "-created_at" (desc), "+name" or "name" (asc).
    pub fn parse(raw: &str) -> Self {
        let raw = raw.trim();
        if let Some(field) = raw.strip_prefix('-') {
            Self::desc(field)
        } else if let Some(field) = raw.strip_prefix('+') {
            Self::asc(field)
        } else {
            Self::asc(raw)
        }
    }

    pub fn sql_direction(&self) -> &'static str {
        match self.direction {
            SortDirection::Asc => "ASC",
            SortDirection::Desc => "DESC",
        }
    }
}

/// Paginated response wrapper.
#[derive(Debug, Clone, Serialize)]
pub struct Paginated<T: Serialize> {
    pub items: Vec<T>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
    pub has_next: bool,
    pub has_previous: bool,
}

impl<T: Serialize> Paginated<T> {
    pub fn new(items: Vec<T>, total: i64, page_query: &PageQuery) -> Self {
        let page_size = page_query.limit();
        let total_pages = if page_size > 0 {
            (total + page_size - 1) / page_size
        } else {
            0
        };
        Self {
            items,
            total,
            page: page_query.page,
            page_size,
            total_pages,
            has_next: page_query.page < total_pages - 1,
            has_previous: page_query.page > 0,
        }
    }
}
