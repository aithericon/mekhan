/**
 * Renderer dispatch for catalogue file metadata (the `FileMetadataView` probe).
 *
 * Priority order:
 *   1. `format` exact match → BY_FORMAT (format-specific override, checked first)
 *   2. `family` match → BY_FAMILY (the broad format family)
 *   3. fallback → GenericFormatMetadata (chips + detail tables, the old markup)
 *
 * Adding a format-specific renderer = one Svelte component + one line in
 * BY_FORMAT. No backend changes — the probe already normalizes `format` and
 * `family` server-side in `catalogue/metadata_view.rs`.
 */

import type { Component } from 'svelte';
import type { MetadataProps, FileMetadataView } from './types';

import GenericFormatMetadata from './GenericFormatMetadata.svelte';
import TabularMetadata from './TabularMetadata.svelte';
import MediaMetadata from './MediaMetadata.svelte';
import ScientificMetadata from './ScientificMetadata.svelte';
import ArchiveMetadata from './ArchiveMetadata.svelte';
import StructuredTextMetadata from './StructuredTextMetadata.svelte';

/**
 * Format-specific overrides, checked FIRST. Keyed on the normalized (lowercased)
 * `format` string. Empty by default — this is the documented extensibility
 * point: add a format-specific renderer = one component + one line here.
 */
export const BY_FORMAT: Record<string, Component<MetadataProps>> = {
	// e.g. 'hdf5': Hdf5Metadata as unknown as Component<MetadataProps>
};

/** Format family → component. The broad dispatch. */
export const BY_FAMILY: Record<string, Component<MetadataProps>> = {
	tabular: TabularMetadata as unknown as Component<MetadataProps>,
	spreadsheet: TabularMetadata as unknown as Component<MetadataProps>,
	image: MediaMetadata as unknown as Component<MetadataProps>,
	audio: MediaMetadata as unknown as Component<MetadataProps>,
	video: MediaMetadata as unknown as Component<MetadataProps>,
	scientific: ScientificMetadata as unknown as Component<MetadataProps>,
	mesh: ScientificMetadata as unknown as Component<MetadataProps>,
	archive: ArchiveMetadata as unknown as Component<MetadataProps>,
	document: StructuredTextMetadata as unknown as Component<MetadataProps>,
	config: StructuredTextMetadata as unknown as Component<MetadataProps>
};

export function pickMetadataRenderer(mv: FileMetadataView): Component<MetadataProps> {
	const format = (mv.format ?? '').toLowerCase();
	const family = (mv.family ?? '').toLowerCase();
	return (
		BY_FORMAT[format] ??
		BY_FAMILY[family] ??
		(GenericFormatMetadata as unknown as Component<MetadataProps>)
	);
}
