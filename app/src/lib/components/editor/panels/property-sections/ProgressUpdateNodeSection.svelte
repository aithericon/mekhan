<script lang="ts">
	// ProgressUpdate: sets the owning HPI process's progress fraction (+ optional
	// message / step counts). Compiles to the typed `process_progress` effect
	// emitting a canonical `StatusDetail::ProgressUpdated`, projected into
	// `config.progress`. No-op outside a named process.
	import type { ProgressUpdateNodeData } from '$lib/types/editor';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import { FormField } from '$lib/components/ui/form-field';
	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';
	import InsertRefButton from './InsertRefButton.svelte';
	import { appendSnippet } from '$lib/editor/append-snippet';

	type Props = {
		data: ProgressUpdateNodeData;
		readonly?: boolean;
		onchange: (data: ProgressUpdateNodeData) => void;
		scope?: ScopeEntry[];
	};

	let { data, readonly = false, onchange, scope = [] }: Props = $props();

	function appendToMessage(snippet: string) {
		onchange({ ...data, message: appendSnippet(data.message, snippet) });
	}

	const pct = $derived(Math.round((data.fraction ?? 0) * 100));

	function clampFraction(raw: string): number {
		const n = parseFloat(raw);
		if (Number.isNaN(n)) return 0;
		return Math.min(1, Math.max(0, n));
	}

	function optInt(raw: string): number | undefined {
		if (raw === '') return undefined;
		const n = parseInt(raw, 10);
		return Number.isNaN(n) ? undefined : n;
	}
</script>

<FormField label="Fraction (0–1) — {pct}%" for="progress-fraction">
	<Input
		id="progress-fraction"
		type="number"
		min={0}
		max={1}
		step={0.05}
		value={data.fraction}
		disabled={readonly}
		data-testid="input-progress-fraction"
		oninput={(e) =>
			onchange({ ...data, fraction: clampFraction((e.currentTarget as HTMLInputElement).value) })}
	/>
</FormField>

<div class="flex gap-2">
	<FormField label="Current step (optional)" for="progress-current">
		<Input
			id="progress-current"
			type="number"
			min={0}
			value={data.currentStep ?? ''}
			disabled={readonly}
			oninput={(e) =>
				onchange({ ...data, currentStep: optInt((e.currentTarget as HTMLInputElement).value) })}
		/>
	</FormField>
	<FormField label="Total steps (optional)" for="progress-total">
		<Input
			id="progress-total"
			type="number"
			min={0}
			value={data.totalSteps ?? ''}
			disabled={readonly}
			oninput={(e) =>
				onchange({ ...data, totalSteps: optInt((e.currentTarget as HTMLInputElement).value) })}
		/>
	</FormField>
</div>

<FormField label="Message (optional)" for="progress-message">
	<Textarea
		id="progress-message"
		value={data.message ?? ''}
		disabled={readonly}
		placeholder={'e.g. Processed {{ count }} rows'}
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
