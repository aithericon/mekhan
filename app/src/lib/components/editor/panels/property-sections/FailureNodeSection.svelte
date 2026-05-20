<script lang="ts">
	// Failure: marks the owning HPI process `failed` with a templated message.
	// Compiles to a `#{ reason }` breadcrumb + the tolerant `process_fail`
	// builtin effect, projected into `config.failure`. The net keeps running
	// to its normal End — this is a process-level marker, not a kill-switch.
	// No-op outside a named process.
	import type { FailureNodeData } from '$lib/types/editor';
	import type { components } from '$lib/api/schema';
	import { FormField } from '$lib/components/ui/form-field';
	import { Textarea } from '$lib/components/ui/textarea';
	import { Input } from '$lib/components/ui/input';
	import { Button } from '$lib/components/ui/button';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';

	type FieldMapping = components['schemas']['FieldMapping'];

	type Props = {
		data: FailureNodeData;
		readonly?: boolean;
		onchange: (data: FailureNodeData) => void;
	};

	let { data, readonly = false, onchange }: Props = $props();

	const errorMappings = $derived(data.errorResultMapping ?? []);

	function setErrorMappings(next: FieldMapping[]) {
		onchange({ ...data, errorResultMapping: next });
	}
	function addErrorMapping() {
		setErrorMappings([...errorMappings, { targetField: '', expression: 'input' }]);
	}
	function updateErrorMapping(idx: number, patch: Partial<FieldMapping>) {
		setErrorMappings(errorMappings.map((m, i) => (i === idx ? { ...m, ...patch } : m)));
	}
	function removeErrorMapping(idx: number) {
		setErrorMappings(errorMappings.filter((_, i) => i !== idx));
	}
</script>

<FormField label="Failure message (optional)" for="failure-message">
	<Textarea
		id="failure-message"
		value={data.failureMessage ?? ''}
		disabled={readonly}
		placeholder={'e.g. Validation failed for invoice {{ invoice_id }}'}
		rows={2}
		data-testid="input-failure-message"
		oninput={(e) => {
			const v = (e.currentTarget as HTMLTextAreaElement).value;
			onchange({ ...data, failureMessage: v === '' ? undefined : v });
		}}
	/>
	<p class="mt-1 text-sm text-muted-foreground">
		Supports <code>{'{{ field }}'}</code> placeholders resolved against the inbound token.
	</p>
</FormField>

<div class="space-y-1.5">
	<div class="flex items-center justify-between">
		<span class="text-sm font-medium text-muted-foreground">Error result (optional)</span>
		{#if !readonly}
			<Button
				variant="ghost"
				size="sm"
				onclick={addErrorMapping}
				data-testid="btn-add-error-result-mapping"
			>
				<Plus class="size-3.5" />
				Add
			</Button>
		{/if}
	</div>
	{#if errorMappings.length === 0}
		<p class="rounded-md border border-dashed border-border/50 p-2 text-sm text-muted-foreground">
			The error envelope still carries the failure message as
			<code>error.reason</code>. Add fields to attach a structured
			<code>error.value</code>.
		</p>
	{:else}
		{#each errorMappings as mapping, i (i)}
			<div class="space-y-1.5 rounded-md border border-border/60 bg-muted/20 p-2">
				<div class="flex items-center gap-2">
					<Input
						type="text"
						value={mapping.targetField}
						disabled={readonly}
						placeholder="error_field"
						data-testid="input-error-result-target"
						oninput={(e) =>
							updateErrorMapping(i, {
								targetField: (e.currentTarget as HTMLInputElement).value
							})}
					/>
					{#if !readonly}
						<Button
							variant="ghost"
							size="sm"
							onclick={() => removeErrorMapping(i)}
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
					placeholder="input.code"
					data-testid="input-error-result-expr"
					oninput={(e) =>
						updateErrorMapping(i, {
							expression: (e.currentTarget as HTMLTextAreaElement).value
						})}
				/>
			</div>
		{/each}
	{/if}
</div>

<p class="text-sm italic text-muted-foreground">
	Marks the process failed but the workflow continues to its End. Effective only within a named
	process (a Start with a Process Name upstream); outside one this node passes the token through
	with no effect.
</p>
