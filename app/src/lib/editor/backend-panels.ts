/**
 * Hand-written map from backend wire name to the Svelte component that
 * renders its config panel. Pairs with the Rust `crate::backends::BACKENDS`
 * registry — the API provides metadata, but Svelte component imports
 * can't be data-driven without defeating Vite chunking, so the import
 * map stays here.
 *
 * Adding a new backend: import its config panel and add the entry.
 *
 * `Partial<Record<…>>` because not every `ExecutionBackendType` is
 * user-authorable: `llm` is an internal lowering target (authored via the
 * Agent node — its degenerate single-shot path emits byte-identical
 * `AutomatedStep(Llm)` IR), so it has no authoring panel. The backend picker
 * (`GET /api/v1/backends`) only lists authorable backends, and
 * `AutomatedStepSection` guards the panel render with `{#if CurrentPanel}`, so
 * a missing entry renders nothing rather than crashing. Every *authorable*
 * backend must still have an entry here.
 */

import type { Component } from 'svelte';
import type { ExecutionBackendType } from '$lib/api/client';

import PythonConfigPanel from '$lib/components/editor/panels/property-sections/automated/PythonConfigPanel.svelte';
import DockerConfigPanel from '$lib/components/editor/panels/property-sections/automated/DockerConfigPanel.svelte';
import ProcessConfigPanel from '$lib/components/editor/panels/property-sections/automated/ProcessConfigPanel.svelte';
import HttpConfigPanel from '$lib/components/editor/panels/property-sections/automated/HttpConfigPanel.svelte';
import FileOpsConfigPanel from '$lib/components/editor/panels/property-sections/automated/FileOpsConfigPanel.svelte';
import KreuzbergConfigPanel from '$lib/components/editor/panels/property-sections/automated/KreuzbergConfigPanel.svelte';
import SmtpConfigPanel from '$lib/components/editor/panels/property-sections/automated/SmtpConfigPanel.svelte';
import PostgresConfigPanel from '$lib/components/editor/panels/property-sections/automated/PostgresConfigPanel.svelte';
import CatalogueQueryConfigPanel from '$lib/components/editor/panels/property-sections/automated/CatalogueQueryConfigPanel.svelte';

// eslint-disable-next-line @typescript-eslint/no-explicit-any
export const BACKEND_PANELS: Partial<Record<ExecutionBackendType, Component<any>>> = {
	python: PythonConfigPanel,
	process: ProcessConfigPanel,
	docker: DockerConfigPanel,
	http: HttpConfigPanel,
	file_ops: FileOpsConfigPanel,
	kreuzberg: KreuzbergConfigPanel,
	smtp: SmtpConfigPanel,
	postgres: PostgresConfigPanel,
	catalogue_query: CatalogueQueryConfigPanel
};
