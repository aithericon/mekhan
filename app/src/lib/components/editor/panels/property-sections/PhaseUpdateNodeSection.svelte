<script lang="ts">
	// PhaseUpdate: marks a named phase on the owning HPI process. Compiles to a
	// `process_log_message` breadcrumb the causality consumer projects into
	// `config.progress.phases`. Only effective downstream of a Start that
	// registered a process (`processName`); otherwise a silent no-op.
	import type { PhaseUpdateNodeData } from '$lib/types/editor';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import { FormField } from '$lib/components/ui/form-field';
	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';
	import * as Select from '$lib/components/ui/select';
	import InsertRefButton from './InsertRefButton.svelte';
	import { appendSnippet } from '$lib/editor/append-snippet';

	type Status = NonNullable<PhaseUpdateNodeData['status']>;

	const statusLabels: Record<Status, string> = {
		running: 'Running',
		completed: 'Completed',
		failed: 'Failed',
		skipped: 'Skipped'
	};

	type Props = {
		data: PhaseUpdateNodeData;
		readonly?: boolean;
		onchange: (data: PhaseUpdateNodeData) => void;
		scope?: ScopeEntry[];
	};

	let { data, readonly = false, onchange, scope = [] }: Props = $props();

	const status = $derived((data.status ?? 'running') as Status);

	function appendToPhaseName(snippet: string) {
		onchange({ ...data, phaseName: appendSnippet(data.phaseName, snippet) });
	}

	function appendToMessage(snippet: string) {
		onchange({ ...data, message: appendSnippet(data.message, snippet) });
	}
</script>

<FormField label="Phase name" for="phase-name">
	<Input
		id="phase-name"
		type="text"
		value={data.phaseName}
		disabled={readonly}
		placeholder="e.g. Validation"
		data-testid="input-phase-name"
		oninput={(e) =>
			onchange({ ...data, phaseName: (e.currentTarget as HTMLInputElement).value })}
	/>
	{#if scope.length > 0}
		<div class="mt-1.5">
			<InsertRefButton
				{scope}
				disabled={readonly}
				oninsert={appendToPhaseName}
			/>
		</div>
	{/if}
</FormField>

<FormField label="Status" for="phase-status">
	<Select.Root
		type="single"
		value={status}
		onValueChange={(v) => {
			if (v) onchange({ ...data, status: v as Status });
		}}
		disabled={readonly}
	>
		<Select.Trigger id="phase-status" class="w-full" disabled={readonly}>
			{statusLabels[status]}
		</Select.Trigger>
		<Select.Content>
			<Select.Item value="running" label="Running" />
			<Select.Item value="completed" label="Completed" />
			<Select.Item value="failed" label="Failed" />
			<Select.Item value="skipped" label="Skipped" />
		</Select.Content>
	</Select.Root>
</FormField>

<FormField label="Message (optional)" for="phase-message">
	<Textarea
		id="phase-message"
		value={data.message ?? ''}
		disabled={readonly}
		placeholder={'e.g. Validating invoice {{ invoice_id }}'}
		rows={2}
		oninput={(e) => {
			const v = (e.currentTarget as HTMLTextAreaElement).value;
			onchange({ ...data, message: v === '' ? undefined : v });
		}}
	/>
	{#if scope.length > 0}
		<div class="mt-1.5">
			<InsertRefButton {scope} disabled={readonly} oninsert={appendToMessage} />
		</div>
	{/if}
	<p class="mt-1 text-sm text-muted-foreground">
		<code>{'{{ ref }}'}</code> placeholders interpolate fields from this node's input scope —
		use the picker above for the in-scope set.
	</p>
</FormField>

<p class="text-sm italic text-muted-foreground">
	Effective only within a named process (a Start with a Process Name upstream). Outside one
	this node passes the token through with no effect.
</p>
