<script lang="ts">
	// Process backend config panel. Fully schema-driven: the field set, types,
	// and widgets come from the backend registry's `configSchema` (the
	// executor `ProcessConfig` JSON Schema) via the shared SchemaForm. No
	// hand-written field ladder — adding a field to `ProcessConfig` surfaces
	// it here automatically.
	import SchemaForm from '../../shared/SchemaForm.svelte';
	import { getCachedBackend } from '$lib/editor/backend-registry.svelte';

	type Props = {
		config: Record<string, unknown>;
		readonly?: boolean;
		onchange: (config: Record<string, unknown>) => void;
	};

	let { config, readonly = false, onchange }: Props = $props();

	const descriptor = $derived(getCachedBackend('process'));
	const schema = $derived((descriptor?.configSchema ?? null) as Record<string, unknown> | null);
	const secretFields = $derived(descriptor?.secretFields ?? []);
</script>

{#if schema}
	<SchemaForm {schema} value={config} {secretFields} {readonly} coerceNumbers {onchange} />
{:else}
	<p class="text-sm text-muted-foreground">Loading backend schema…</p>
{/if}
