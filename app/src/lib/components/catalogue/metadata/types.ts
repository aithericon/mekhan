import type { components } from '$lib/api/schema';
import type { SchemaNode } from '$lib/schema/model';

/**
 * The normalized, UI-facing probe metadata (built server-side in
 * `catalogue/metadata_view.rs`). Format-specific metadata renderers consume
 * this view; the catalogue card derives it from `entry.metadata_view`.
 */
export type FileMetadataView = components['schemas']['FileMetadataView'];

export interface MetadataProps {
	mv: FileMetadataView;
	/**
	 * Per-column schema trees recovered from the probe's raw nested `DataType`
	 * (the entry's `file_metadata.columns`), keyed by column name. Lets renderers
	 * show a real expandable type tree for complex (struct/list) columns instead
	 * of the flat, truncated humanized `column.data_type` string. Absent for
	 * legacy / pre-probe rows — renderers fall back to the humanized string.
	 */
	columnSchemas?: Map<string, SchemaNode>;
	onSchemaClick?: (digest: string) => void;
}
