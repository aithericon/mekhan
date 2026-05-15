<script lang="ts">
	// Retry policy for an automated step. On execution failure/timeout the
	// compiler re-dispatches (a fresh executor submit) while
	// attempts < maxRetries — immediately, or after a fixed / exponential
	// (base << attempt) delay — then routes the exhausted token to the node's
	// error output.
	import type { AutomatedStepNodeData } from '$lib/types/editor';
	import { Input } from '$lib/components/ui/input';
	import { FormField } from '$lib/components/ui/form-field';

	type Props = {
		data: AutomatedStepNodeData;
		readonly?: boolean;
		onchange: (data: AutomatedStepNodeData) => void;
	};

	let { data, readonly = false, onchange }: Props = $props();

	const policy = $derived(
		data.retryPolicy ?? { maxRetries: 3, backoff: 'immediate' as const, baseDelayMs: 0 }
	);

	function patch(p: Partial<typeof policy>) {
		onchange({ ...data, retryPolicy: { ...policy, ...p } });
	}
</script>

<div class="space-y-3 border-t border-border/40 pt-3">
	<span class="text-xs font-medium text-muted-foreground">Retry policy</span>

	<FormField label="Max retries" for="retry-max">
		<Input
			id="retry-max"
			type="number"
			min={0}
			value={policy.maxRetries}
			disabled={readonly}
			data-testid="input-retry-max"
			oninput={(e) =>
				patch({
					maxRetries: Math.max(
						0,
						parseInt((e.currentTarget as HTMLInputElement).value) || 0
					)
				})}
		/>
	</FormField>

	<FormField label="Backoff" for="retry-backoff">
		<select
			id="retry-backoff"
			class="h-9 w-full rounded-md border border-input bg-background px-2 text-sm"
			disabled={readonly}
			value={policy.backoff}
			data-testid="select-retry-backoff"
			onchange={(e) =>
				patch({
					backoff: (e.currentTarget as HTMLSelectElement).value as
						| 'immediate'
						| 'fixed'
						| 'exponential'
				})}
		>
			<option value="immediate">Immediate</option>
			<option value="fixed">Fixed delay</option>
			<option value="exponential">Exponential backoff</option>
		</select>
	</FormField>

	{#if policy.backoff !== 'immediate'}
		<FormField label="Base delay (ms)" for="retry-delay">
			<Input
				id="retry-delay"
				type="number"
				min={0}
				value={policy.baseDelayMs}
				disabled={readonly}
				data-testid="input-retry-delay"
				oninput={(e) =>
					patch({
						baseDelayMs: Math.max(
							0,
							parseInt((e.currentTarget as HTMLInputElement).value) || 0
						)
					})}
			/>
		</FormField>
		<p class="text-[10px] italic text-muted-foreground">
			{policy.backoff === 'exponential'
				? 'Delay = base × 2^attempt (0-based).'
				: 'Fixed delay before every retry.'}
		</p>
	{/if}
	<p class="text-[10px] italic text-muted-foreground">
		After retries are exhausted the token flows to the step's error output.
	</p>
</div>
