<script lang="ts">
	// Shared dropdown for binding an AutomatedStep to a workspace resource.
	//
	// Backends that follow the `resource_alias` pattern (SMTP, LLM,
	// file-ops S3 / GCS / AzBlob, …) drop this in once per binding point;
	// the picker takes care of loading, loading/empty/error states, and
	// the bound/unbound helper hint.
	//
	// `resourceType={null}` renders nothing — useful for provider-scoped
	// panels (e.g. the LLM panel only shows the picker when provider
	// maps to a workspace resource type).

	import { Input as _Input } from '$lib/components/ui/input';
	import { FormField } from '$lib/components/ui/form-field';
	import * as Select from '$lib/components/ui/select';
	import { listResources, type ResourceSummary } from '$lib/api/resources';

	type Props = {
		resourceType: string | null;
		selected: string;
		onChange: (alias: string) => void;
		label?: string;
		readonly?: boolean;
		testId?: string;
		/// Friendly singular noun shown in the empty-state message
		/// (e.g. "SMTP", "OpenAI", "S3"). Defaults to the resource type.
		typeLabel?: string;
	};

	let {
		resourceType,
		selected,
		onChange,
		label = 'Credentials resource',
		readonly = false,
		testId,
		typeLabel
	}: Props = $props();

	let resources = $state<ResourceSummary[]>([]);
	let loading = $state(false);
	let error = $state<string | null>(null);
	let lastLoadedType: string | null = null;

	async function load(type: string | null) {
		if (!type) {
			resources = [];
			error = null;
			return;
		}
		loading = true;
		error = null;
		try {
			const page = await listResources({ resource_type: type, perPage: 200 });
			resources = page.items;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load resources';
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		if (resourceType !== lastLoadedType) {
			lastLoadedType = resourceType;
			load(resourceType);
		}
	});

	function selectedLabel(): string {
		if (!selected) {
			return loading ? 'Loading…' : 'None — provide credentials inline';
		}
		const found = resources.find((r) => r.path === selected);
		return found ? `${found.path} — ${found.display_name}` : selected;
	}

	const friendly = $derived(typeLabel ?? resourceType ?? '');
</script>

{#if resourceType}
	<div class="space-y-1.5">
		<FormField {label} for={testId ?? 'resource-picker'}>
			<Select.Root
				type="single"
				value={selected}
				onValueChange={(v) => onChange(v ?? '')}
				disabled={readonly || loading}
			>
				<Select.Trigger disabled={readonly || loading} data-testid={testId}>
					<span class="truncate text-sm">{selectedLabel()}</span>
				</Select.Trigger>
				<Select.Content>
					<Select.Item value="" label="None — provide credentials inline" />
					{#each resources as r (r.id)}
						<Select.Item value={r.path} label={`${r.path} — ${r.display_name}`} />
					{/each}
				</Select.Content>
			</Select.Root>
		</FormField>
		{#if error}
			<p class="text-sm text-destructive">{error}</p>
		{:else if resources.length === 0 && !loading}
			<p class="text-sm italic text-muted-foreground">
				No {friendly} resources configured in this workspace. Add one under
				<code class="font-mono">/resources</code> to share credentials across steps.
			</p>
		{:else if selected}
			<p class="text-sm italic text-muted-foreground">
				Credentials come from the resource. Fields below override them per step.
			</p>
		{/if}
	</div>
{/if}
