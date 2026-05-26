/**
 * Hand-written map from backend wire name to the Svelte component that
 * renders its config panel. Pairs with the Rust `crate::backends::BACKENDS`
 * registry — the API provides metadata, but Svelte component imports
 * can't be data-driven without defeating Vite chunking, so the import
 * map stays here.
 *
 * Adding a new backend: import its config panel and add the entry.
 * Compile-time exhaustiveness via `Record<ExecutionBackendType, …>`
 * makes "added a backend but forgot the panel" a build error.
 */

import type { Component } from 'svelte';
import type { ExecutionBackendType } from '$lib/api/client';

import PythonConfigPanel from '$lib/components/editor/panels/property-sections/automated/PythonConfigPanel.svelte';
import DockerConfigPanel from '$lib/components/editor/panels/property-sections/automated/DockerConfigPanel.svelte';
import ProcessConfigPanel from '$lib/components/editor/panels/property-sections/automated/ProcessConfigPanel.svelte';
import HttpConfigPanel from '$lib/components/editor/panels/property-sections/automated/HttpConfigPanel.svelte';
import LlmConfigPanel from '$lib/components/editor/panels/property-sections/automated/LlmConfigPanel.svelte';
import FileOpsConfigPanel from '$lib/components/editor/panels/property-sections/automated/FileOpsConfigPanel.svelte';
import KreuzbergConfigPanel from '$lib/components/editor/panels/property-sections/automated/KreuzbergConfigPanel.svelte';
import SmtpConfigPanel from '$lib/components/editor/panels/property-sections/automated/SmtpConfigPanel.svelte';
import CatalogueQueryConfigPanel from '$lib/components/editor/panels/property-sections/automated/CatalogueQueryConfigPanel.svelte';

// eslint-disable-next-line @typescript-eslint/no-explicit-any
export const BACKEND_PANELS: Record<ExecutionBackendType, Component<any>> = {
	python: PythonConfigPanel,
	process: ProcessConfigPanel,
	docker: DockerConfigPanel,
	http: HttpConfigPanel,
	llm: LlmConfigPanel,
	file_ops: FileOpsConfigPanel,
	kreuzberg: KreuzbergConfigPanel,
	smtp: SmtpConfigPanel,
	catalogue_query: CatalogueQueryConfigPanel
};
