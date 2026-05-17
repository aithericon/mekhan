<script lang="ts">
	// End: optional success-result binding. Each row's expression is Rhai over
	// the inbound token; together they assemble the `value` of the success
	// envelope (`{ ok: true, value }`) stamped onto the terminal token's
	// `exit_code`. Empty (the default) = no envelope, terminal token
	// byte-identical to pre-feature behavior (instance `result` stays null).
	import type { EndNodeData } from '$lib/types/editor';
	import type { components } from '$lib/api/schema';
	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';
	import { Button } from '$lib/components/ui/button';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';

	type FieldMapping = components['schemas']['FieldMapping'];

	type Props = {
		data: EndNodeData;
		readonly?: boolean;
		onchange: (data: EndNodeData) => void;
	};

	let { data, readonly = false, onchange }: Props = $props();

	const mappings = $derived(data.resultMapping ?? []);

	function setMappings(next: FieldMapping[]) {
		onchange({ ...data, resultMapping: next });
	}
	function addMapping() {
		setMappings([...mappings, { targetField: '', expression: 'input' }]);
	}
	function updateMapping(idx: number, patch: Partial<FieldMapping>) {
		setMappings(mappings.map((m, i) => (i === idx ? { ...m, ...patch } : m)));
	}
	function removeMapping(idx: number) {
		setMappings(mappings.filter((_, i) => i !== idx));
	}
</script>

<div class="space-y-1.5">
	<div class="flex items-center justify-between">
		<span class="text-xs font-medium text-muted-foreground">Result mapping</span>
		{#if !readonly}
			<Button variant="ghost" size="sm" onclick={addMapping} data-testid="btn-add-result-mapping">
				<Plus class="size-3.5" />
				Add
			</Button>
		{/if}
	</div>

	{#if mappings.length === 0}
		<p class="rounded-md border border-dashed border-border/50 p-2 text-[11px] text-muted-foreground">
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
				<Textarea
					value={mapping.expression}
					disabled={readonly}
					rows={2}
					placeholder="input.total"
					data-testid="input-result-expr"
					oninput={(e) =>
						updateMapping(i, {
							expression: (e.currentTarget as HTMLTextAreaElement).value
						})}
				/>
			</div>
		{/each}
	{/if}

	<p class="text-[10px] text-muted-foreground">
		Each expression is Rhai evaluated against the inbound token
		(<code>input.&lt;field&gt;</code>). A <code>Failure</code> node upstream
		takes precedence — its error envelope is preserved instead of overwritten.
	</p>
</div>
