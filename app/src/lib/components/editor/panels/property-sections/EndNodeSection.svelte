<script lang="ts">
	// End: optional success-result binding. Each row picks an in-scope ref via
	// RefPicker (the compiler synthesizes a read-arc into the producer's parked
	// data place) and assembles the `value` of the success envelope
	// (`{ ok: true, value }`) stamped onto the terminal token's `exit_code`.
	// `targetField` defaults to the picked field name but can be renamed.
	// Empty (the default) = no envelope, terminal token byte-identical to
	// pre-feature behavior (instance `result` stays null).
	import type { EndNodeData } from '$lib/types/editor';
	import type { components } from '$lib/api/schema';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import { Input } from '$lib/components/ui/input';
	import { Button } from '$lib/components/ui/button';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import RefPicker from './RefPicker.svelte';

	type FieldMapping = components['schemas']['FieldMapping'];

	type Props = {
		data: EndNodeData;
		readonly?: boolean;
		onchange: (data: EndNodeData) => void;
		scope?: ScopeEntry[];
	};

	let { data, readonly = false, onchange, scope = [] }: Props = $props();

	const mappings = $derived(data.resultMapping ?? []);

	function setMappings(next: FieldMapping[]) {
		onchange({ ...data, resultMapping: next });
	}
	function addMapping() {
		setMappings([...mappings, { targetField: '', expression: '' }]);
	}
	function updateMapping(idx: number, patch: Partial<FieldMapping>) {
		setMappings(mappings.map((m, i) => (i === idx ? { ...m, ...patch } : m)));
	}
	function removeMapping(idx: number) {
		setMappings(mappings.filter((_, i) => i !== idx));
	}
	// Picking a ref replaces the expression with the qualified `<slug>.<field>`
	// (the compiler turns this into a read-arc). The targetField is auto-filled
	// from the picked field name only when blank — once the user has renamed
	// the output key, a subsequent ref change leaves their rename intact.
	function pickRef(idx: number, entry: ScopeEntry) {
		const current = mappings[idx];
		updateMapping(idx, {
			expression: entry.qualified,
			...(current?.targetField ? {} : { targetField: entry.field })
		});
	}
</script>

<div class="space-y-1.5">
	<div class="flex items-center justify-between">
		<span class="text-sm font-medium text-muted-foreground">Result mapping</span>
		{#if !readonly}
			<Button variant="ghost" size="sm" onclick={addMapping} data-testid="btn-add-result-mapping">
				<Plus class="size-3.5" />
				Add
			</Button>
		{/if}
	</div>

	{#if mappings.length === 0}
		<p class="rounded-md border border-dashed border-border/50 p-2 text-sm text-muted-foreground">
			No result. The workflow completes with no structured return — fully
			backward-compatible. Add one or more fields to build the success envelope
			(<code>{'{ ok: true, value }'}</code>) returned to callers.
		</p>
	{:else}
		{#each mappings as mapping, i (i)}
			<div class="space-y-1.5 rounded-md border border-border/60 bg-muted/20 p-2">
				<div class="flex items-center gap-2">
					<Input
						type="text"
						value={mapping.targetField}
						disabled={readonly}
						placeholder="result_field"
						data-testid="input-result-target"
						oninput={(e) =>
							updateMapping(i, {
								targetField: (e.currentTarget as HTMLInputElement).value
							})}
					/>
					{#if !readonly}
						<Button
							variant="ghost"
							size="sm"
							onclick={() => removeMapping(i)}
							aria-label="Remove"
						>
							<Trash2 class="size-3.5" />
						</Button>
					{/if}
				</div>
				<RefPicker
					{scope}
					disabled={readonly}
					selected={mapping.expression || undefined}
					placeholder="Pick source field…"
					onpick={(entry) => pickRef(i, entry)}
				/>
			</div>
		{/each}
	{/if}

	<p class="text-sm text-muted-foreground">
		Each row borrows a field from upstream (the compiler synthesizes a
		non-consuming read-arc) and assembles the success envelope. Rename the
		left field to publish under a different key. A <code>Failure</code> node
		upstream takes precedence — its error envelope is preserved instead of
		overwritten.
	</p>
</div>
