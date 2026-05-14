// @aithericon/hpi-ui — HPI task display components for Svelte 5

// Types
export type {
	TaskFieldKind,
	TaskField,
	TaskBlock,
	TaskStep,
	HumanTask,
	ProcessState,
	ProcessTimelineEntry,
	ProcessStepDef,
	SignatureValue,
	SignatureMode,
	SignatureAudit,
	DownloadItem,
	ChartType,
	ChartSeries,
	ChartBlockData,
	TaskSink,
	TaskSinkEvent
} from './types';

export { TASK_FIELD_KINDS, CHART_TYPES } from './types';

// Display components
export { default as BlockImage } from './components/block-image.svelte';
export { default as BlockPdf } from './components/block-pdf.svelte';
export { default as BlockRenderer } from './components/block-renderer.svelte';
export { default as Callout } from './components/callout.svelte';
export { default as DataTable } from './components/data-table.svelte';
export { default as DownloadCard } from './components/download-card.svelte';
export { default as FieldDisplay } from './components/field-display.svelte';
export { default as ProcessBanner } from './components/process-banner.svelte';

// Utilities
export { setLinkId, getLinkId, withLinkParam } from './components/link-context';
export { displaySize, cn } from './utils';
