<script lang="ts">
	import { Input } from '$lib/components/ui/input';
	import { FormField } from '$lib/components/ui/form-field';

	// Mirrors the sibling backend config panels' contract so
	// AutomatedStepSection can dispatch identically. Builds the ADR-17
	// convenience query token the engine `catalogue_lookup` effect accepts
	// (top-level category / source_net / source_process_id / search / sort_by /
	// limit). Generic typed `filters` are authored as raw JSON (advanced).
	type Props = {
		config: Record<string, unknown>;
		readonly?: boolean;
		onchange: (config: Record<string, unknown>) => void;
	};

	let { config, readonly = false, onchange }: Props = $props();

	const str = (k: string) => (typeof config[k] === 'string' ? (config[k] as string) : '');
	const num = (k: string, d: number) =>
		typeof config[k] === 'number' ? (config[k] as number) : d;

	function set(key: string, value: unknown) {
		const next = { ...config };
		if (value === '' || value == null) delete next[key];
		else next[key] = value;
		onchange(next);
	}
</script>

<div class="space-y-3" data-testid="catalogue-query-config">
	<FormField label="Category" for="cat-category">
		<Input
			id="cat-category"
			placeholder="e.g. model, dataset"
			value={str('category')}
			disabled={readonly}
			data-testid="input-cat-category"
			oninput={(e) => set('category', (e.currentTarget as HTMLInputElement).value)}
		/>
	</FormField>
	<FormField label="Source net" for="cat-source-net">
		<Input
			id="cat-source-net"
			placeholder="optional — filter by producing net"
			value={str('source_net')}
			disabled={readonly}
			oninput={(e) => set('source_net', (e.currentTarget as HTMLInputElement).value)}
		/>
	</FormField>
	<FormField label="Search" for="cat-search">
		<Input
			id="cat-search"
			placeholder="optional free-text"
			value={str('search')}
			disabled={readonly}
			oninput={(e) => set('search', (e.currentTarget as HTMLInputElement).value)}
		/>
	</FormField>
	<div class="grid grid-cols-2 gap-2">
		<FormField label="Sort by" for="cat-sort">
			<Input
				id="cat-sort"
				placeholder="e.g. created_at"
				value={str('sort_by')}
				disabled={readonly}
				oninput={(e) => set('sort_by', (e.currentTarget as HTMLInputElement).value)}
			/>
		</FormField>
		<FormField label="Limit" for="cat-limit">
			<Input
				id="cat-limit"
				type="number"
				min="1"
				value={num('limit', 50)}
				disabled={readonly}
				data-testid="input-cat-limit"
				oninput={(e) =>
					set('limit', parseInt((e.currentTarget as HTMLInputElement).value, 10) || 50)}
			/>
		</FormField>
	</div>
	<p class="text-sm text-muted-foreground">
		Point-in-time read of the data catalogue (no executor job). Returns
		<code>artifacts</code>, <code>total_count</code>, <code>source_process_ids</code>.
	</p>
</div>
