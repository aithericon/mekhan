// Backend-defaulted output port shape for AutomatedStep, sourced from the
// `GET /api/backends` registry cache. `+layout.svelte` warms the cache on
// app mount so synchronous callers (the "Reset to backend default" button)
// see populated data on first paint.
//
// If the registry hasn't loaded yet (deep-link before +layout.onMount
// resolves, or a fetch error), `defaultOutputPort` returns an empty port
// — the user sees the reset button briefly emit nothing instead of the
// canonical fields. AutomatedStepSection.svelte fires `loadBackends()`
// in its own onMount as a safety net, so this window is short.

import type { components } from '$lib/api/schema';
import { getCachedBackend } from './backend-registry.svelte';

type Port = components['schemas']['Port'];
type ExecutionBackendType = components['schemas']['ExecutionBackendType'];

export function defaultOutputPort(backend: ExecutionBackendType): Port {
	const descriptor = getCachedBackend(backend);
	return descriptor?.defaultOutputPort ?? emptyOutputPort();
}

export function emptyOutputPort(): Port {
	return { id: 'out', label: 'Output', fields: [] };
}
