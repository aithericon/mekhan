<script lang="ts">
	// Failure: marks the owning HPI process `failed` with a templated message.
	// Compiles to a `#{ reason }` breadcrumb + the tolerant `process_fail`
	// builtin effect, projected into `config.failure`. The net keeps running
	// to its normal End — this is a process-level marker, not a kill-switch.
	// No-op outside a named process.
	import type { FailureNodeData } from '$lib/types/editor';
	import { FormField } from '$lib/components/ui/form-field';
	import { Textarea } from '$lib/components/ui/textarea';

	type Props = {
		data: FailureNodeData;
		readonly?: boolean;
		onchange: (data: FailureNodeData) => void;
	};

	let { data, readonly = false, onchange }: Props = $props();
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
	<p class="mt-1 text-[10px] text-muted-foreground">
		Supports <code>{'{{ field }}'}</code> placeholders resolved against the inbound token.
	</p>
</FormField>

<p class="text-[10px] italic text-muted-foreground">
	Marks the process failed but the workflow continues to its End. Effective only within a named
	process (a Start with a Process Name upstream); outside one this node passes the token through
	with no effect.
</p>
