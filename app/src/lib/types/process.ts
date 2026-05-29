/**
 * Frontend-only process shapes. The wire types (`HpiProcess`, `HpiTask`,
 * `HpiMetric`, `HpiMetricSummary`, `HpiLog`, `ProcessDetail`, `ProcessStats`)
 * plus the paginated envelopes (`PaginatedProcesses`, `PaginatedLogs`,
 * `PaginatedArtifacts`) are exported from `$lib/api/client` — import them
 * from there.
 *
 * The canonical phase/progress model (`PhaseStatus`, `Phase`, `Progress`) is
 * generated from `aithericon_executor_domain::{PhaseStatus, Phase, Progress}`
 * (stored verbatim by the mekhan ingest projector at
 * `process.config.progress`) and likewise lives in `$lib/api/client`. It is
 * re-exported here for the existing import sites; no longer hand-mirrored.
 *
 * This module is now scoped to `ProcessTimelineEntry`, a frontend-only shape
 * with no Rust counterpart.
 */

export type { PhaseStatus, Phase, Progress } from '$lib/api/client';

export interface ProcessTimelineEntry {
	step: string;
	label: string;
	status: 'pending' | 'running' | 'completed' | 'failed' | 'skipped';
	started_at?: string;
	completed_at?: string;
	duration_ms?: number;
}
