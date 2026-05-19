<script lang="ts">
	import type { components } from '$lib/api/schema';
	import Plus from '@lucide/svelte/icons/plus';
	import { Button } from '$lib/components/ui/button';
	import PortFieldEditor from './PortFieldEditor.svelte';

	// Editor for a single named Port (its `fields` list). Used for editable
	// ports (Start.initial, AutomatedStep.output in Phase 2, Scope inputs in
	// Phase 4). Derived ports (HumanTask outputs, Decision branches) should
	// render read-only — pass `readonly`.

	type Port = components['schemas']['Port'];
	type PortField = components['schemas']['PortField'];

	type Props = {
		port: Port;
		readonly?: boolean;
		title?: string;
		emptyHint?: string;
		onchange: (port: Port) => void;
	};

	let {
		port,
		readonly = false,
		title = 'Fields',
		emptyHint = 'No fields declared. The token on this port carries no typed data.',
		onchange
	}: Props = $props();

	function updateField(index: number, field: PortField) {
		const next = [...(port.fields ?? [])];
		next[index] = field;
		onchange({ ...port, fields: next });
	}

	function removeField(index: number) {
		const next = [...(port.fields ?? [])];
		next.splice(index, 1);
		onchange({ ...port, fields: next });
	}

	function addField() {
		const next = [
			...(port.fields ?? []),
			{
				name: '',
				label: '',
				kind: 'text' as const,
				required: false
			}
		];
		onchange({ ...port, fields: next });
	}
</script>

<div class="space-y-2">
	<div class="flex items-center justify-between">
		<span class="text-sm font-medium text-muted-foreground">{title}</span>
		{#if !readonly}
			<Button variant="ghost" size="sm" onclick={addField} class="h-7 gap-1 px-2 text-sm">
				<Plus class="size-3.5" />
				Add field
			</Button>
		{/if}
	</div>

	{#if (port.fields ?? []).length === 0}
		<p class="rounded-md border border-dashed border-border/50 p-3 text-sm text-muted-foreground">
			{emptyHint}
		</p>
	{:else}
		<div class="space-y-1.5">
			{#each port.fields ?? [] as field, i (i)}
				<PortFieldEditor
					{field}
					{readonly}
					onchange={(f) => updateField(i, f)}
					onremove={() => removeField(i)}
				/>
			{/each}
		</div>
	{/if}
</div>
