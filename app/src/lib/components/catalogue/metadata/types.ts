import type { components } from '$lib/api/schema';

/**
 * The normalized, UI-facing probe metadata (built server-side in
 * `catalogue/metadata_view.rs`). Format-specific metadata renderers consume
 * this view; the catalogue card derives it from `entry.metadata_view`.
 */
export type FileMetadataView = components['schemas']['FileMetadataView'];

export interface MetadataProps {
	mv: FileMetadataView;
	onSchemaClick?: (digest: string) => void;
}
