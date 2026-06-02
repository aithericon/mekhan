<script lang="ts">
	// Shared dropdown for binding a node to a scope-visible asset (docs/20 §5).
	// Mirrors ResourcePicker: a Select over `listAssets`, emitting the chosen
	// asset's flat `ref_key`. The binding is recorded on the node as an
	// `assetBinding` { alias, refKey } and lowers to staged InputDeclarations at
	// compile time; the version is pinned at launch (like resource_pins).
	//
	// `scope` resolves downward-visibility for the asset list. When the editor
	// has a template id in context it can pass `template:<id>`; otherwise the
	// caller's workspace assets are listed.
	import { FormField } from '$lib/components/ui/form-field';
	import * as Select from '$lib/components/ui/select';
	import { listAssets, listAssetTypes, type AssetSummary, type ScopeContext } from '$lib/api/assets';

	type Props = {
		/** Currently-bound asset ref-key, or '' for unbound. */
		selected: string;
		onChange: (refKey: string) => void;
		/** Optional: restrict the list to one asset type id. */
		typeId?: string | null;
		scope?: ScopeContext;
		label?: string;
		readonly?: boolean;
		testId?: string;
	};

	let {
		selected,
		onChange,
		typeId = null,
		scope = { kind: 'workspace' },
		label = 'Asset',
		readonly = false,
		testId
	}: Props = $props();

	let assets = $state<AssetSummary[]>([]);
	let typeNames = $state<Record<string, string>>({});
	let loading = $state(false);
	let error = $state<string | null>(null);
	let lastKey: string | null = null;

	const scopeKey = $derived.by(() => {
		const s = scope.kind === 'workspace' ? 'workspace' : `${scope.kind}:${scope.id}`;
		return `${s}|${typeId ?? ''}`;
	});

	async function load() {
		loading = true;
		error = null;
		try {
			const [page, t] = await Promise.all([
				listAssets({ scope, type_id: typeId ?? undefined, perPage: 200 }),
				listAssetTypes({ scope, perPage: 200 })
			]);
			assets = page.items;
			typeNames = Object.fromEntries(t.items.map((x) => [x.id, x.name]));
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load assets';
			assets = [];
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		if (scopeKey !== lastKey) {
			lastKey = scopeKey;
			load();
		}
	});

	function labelFor(a: AssetSummary): string {
		const tn = typeNames[a.type_id];
		return tn ? `${a.ref_key} — ${tn}` : a.ref_key;
	}

	function selectedLabel(): string {
		if (!selected) return loading ? 'Loading…' : 'None';
		const found = assets.find((a) => a.ref_key === selected);
		return found ? labelFor(found) : selected;
	}
</script>

<div class="space-y-1.5">
	<FormField {label} for={testId ?? 'asset-picker'}>
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
				<Select.Item value="" label="None" />
				{#each assets as a (a.id)}
					<Select.Item value={a.ref_key} label={labelFor(a)} />
				{/each}
			</Select.Content>
		</Select.Root>
	</FormField>
	{#if error}
		<p class="text-sm text-destructive">{error}</p>
	{:else if assets.length === 0 && !loading}
		<p class="text-sm italic text-muted-foreground">
			No assets visible in this scope. Curate one under
			<code class="font-mono">/assets</code> to stage it as input.
		</p>
	{:else if selected}
		<p class="text-sm italic text-muted-foreground">
			Staged as <code class="font-mono">{selected}.json</code>; the node reads it as an ordinary input.
		</p>
	{/if}
</div>
