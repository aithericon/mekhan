/**
 * Known scheduler job templates an AutomatedStep can target when its
 * deployment model is `scheduled`. A small static set kept as a TS twin (like
 * `automated-ports.ts`) so the editor stays offline-capable; swap for a fetch
 * behind the same `{ value, label }[]` contract if it ever needs to be dynamic.
 * Values must match the cluster's registered parameterized job ids.
 */
export const GPU_JOB_TEMPLATES = [
	{ value: 'petri-mumax3-worker', label: 'mumax3 (micromagnetics, GPU)' },
	{ value: 'petri-executor-gpu-worker', label: 'Generic GPU executor' }
] as const;

export type JobTemplate = (typeof GPU_JOB_TEMPLATES)[number]['value'];
