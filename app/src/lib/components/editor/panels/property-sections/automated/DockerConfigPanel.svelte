<script lang="ts">
	// Docker backend config panel. Fully schema-driven off the backend
	// registry's `configSchema` (the executor `DockerConfig` JSON Schema) via
	// the shared SchemaForm — arrays (command / entrypoint / extra_volumes)
	// render as string-list editors, `env` as a key/value editor, `pull_policy`
	// as an enum select, and the nested `resource_limits` object as a labeled
	// sub-form.
	import SchemaForm from '../../shared/SchemaForm.svelte';
	import { getCachedBackend } from '$lib/editor/backend-registry.svelte';

	type Props = {
		config: Record<string, unknown>;
		readonly?: boolean;
		onchange: (config: Record<string, unknown>) => void;
	};

	let { config, readonly = false, onchange }: Props = $props();

	const descriptor = $derived(getCachedBackend('docker'));
	const schema = $derived((descriptor?.configSchema ?? null) as Record<string, unknown> | null);
	const secretFields = $derived(descriptor?.secretFields ?? []);
</script>

{#if schema}
	<SchemaForm {schema} value={config} {secretFields} {readonly} coerceNumbers {onchange} />
{:else}
	<p class="text-sm text-muted-foreground">Loading backend schema…</p>
{/if}
