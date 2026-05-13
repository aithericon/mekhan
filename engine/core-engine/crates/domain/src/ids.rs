use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
pub struct PlaceId(pub String);

impl PlaceId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    pub fn named(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl Default for PlaceId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for PlaceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
pub struct TransitionId(pub String);

impl TransitionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    pub fn named(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl Default for TransitionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TransitionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
pub struct TokenId(pub Uuid);

impl TokenId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

impl Default for TokenId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TokenId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
pub struct ArcId(pub Uuid);

impl ArcId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

impl Default for ArcId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ArcId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
