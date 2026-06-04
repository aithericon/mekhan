<script lang="ts">
	import type { ModelSetView } from '$lib/api/models';
	// Internal model-pool picker. Drop-in replacement for the free-text model
	// Input when provider === 'internal'. Lists the loaded-set projection
	// (`GET /api/v1/models`) and offers ONLY the models the control plane reports
	// as `available` (the AND-gate). A self-hosted model id can therefore never
	// be free-typed into an internal binding — it is picked from what the pool
	// actually serves.
	//
	// `resourceAlias` (the bound `internal_llm` resource) is accepted for parity
	// with ResourcePicker and future per-resource scoping; the P1 loaded-set is
	// workspace-wide so the list is not yet filtered by it.

	import * as Select from '$lib/components/ui/select';
	import { FormField } from '$lib/components/ui/form-field';
	import { listLoadedModels } from '$lib/api/models';

	type Props = {
		selected: string;
		onChange: (modelId: string) => void;
		/// The bound `internal_llm` resource alias (informational for now).
		resourceAlias?: string;
		label?: string;
		readonly?: boolean;
		testId?: string;
	};

	let {
		selected,
		onChange,
		resourceAlias = '',
		label = 'Model',
		readonly = false,
		testId = 'model-picker'
	}: Props = $props();

	let models = $state<ModelSetView[]>([]);
	let loading = $state(false);
	let error = $state<string | null>(null);
	let loaded = $state(false);

	async function load() {
		loading = true;
		error = null;
		try {
			models = await listLoadedModels();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load models';
			models = [];
		} finally {
			loading = false;
			loaded = true;
		}
	}

	$effect(() => {
		if (!loaded) load();
	});

	const available = $derived(models.filter((m) => m.available));

	function selectedLabel(): string {
		if (loading && !loaded) return 'Loading…';
		if (!selected) return 'Select a model…';
		return selected;
	}
</script>

<div class="space-y-1.5">
	<FormField {label} for={testId}>
		<Select.Root
			type="single"
			value={selected}
			onValueChange={(v) => onChange(v ?? '')}
			disabled={readonly || loading || available.length === 0}
		>
			<Select.Trigger
				disabled={readonly || loading || available.length === 0}
				data-testid={testId}
			>
				<span class="truncate font-mono text-sm">{selectedLabel()}</span>
			</Select.Trigger>
			<Select.Content>
				{#each available as m (m.model_id)}
					<Select.Item
						value={m.model_id}
						label={m.base ? `${m.model_id} (LoRA · ${m.base})` : m.model_id}
					/>
				{/each}
			</Select.Content>
		</Select.Root>
	</FormField>
	{#if error}
		<p class="text-sm text-destructive">{error}</p>
	{:else if available.length === 0 && loaded}
		<p class="text-sm italic text-muted-foreground">
			No models are currently loaded in the pool. An operator must load a model under
			<code class="font-mono">/control-plane</code> before it can be selected here.
		</p>
	{:else if selected}
		<p class="text-sm italic text-muted-foreground">
			Inference routes through the internal pool router. Credentials and endpoint are fixed by
			the bound resource and cannot be overridden.
		</p>
	{/if}
</div>
