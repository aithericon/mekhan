pub mod doc_ops;
pub mod manager;
pub mod persistence;
pub mod room;

/// The kind of document a Yjs room/persistence partition holds.
///
/// Both `Graph` (workflow-template canvases) and `Page` (free-form rich-text
/// pages) share one opaque-UUID-keyed Yjs stack; `DocKind` is the only
/// template-vs-page discriminator that flows to the DB write seam (stamped on
/// the room at the route boundary — "plan A"). Internal only — NOT a
/// `ToSchema`, never crosses the OpenAPI surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    Graph,
    Page,
}

impl DocKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DocKind::Graph => "graph",
            DocKind::Page => "page",
        }
    }
}
