<script lang="ts">
	import type { TriggerNodeData } from '$lib/types/editor';
	import type { components } from '$lib/api/schema';
	import { FormField } from '$lib/components/ui/form-field';
	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';
	import { Button } from '$lib/components/ui/button';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';

	type FieldMapping = components['schemas']['FieldMapping'];

	type Props = {
		data: TriggerNodeData;
		readonly?: boolean;
		onchange: (data: TriggerNodeData) => void;
	};

	let { data, readonly = false, onchange }: Props = $props();

	const source = $derived(data.source);
	const sourceKind = $derived(source?.kind ?? 'manual');
	const mappings = $derived(data.payloadMapping ?? []);
	const enabled = $derived(data.enabled ?? false);

	function update<K extends keyof TriggerNodeData>(key: K, value: TriggerNodeData[K]) {
		onchange({ ...data, [key]: value });
	}

	function updateSourceKind(kind: TriggerNodeData['source']['kind']) {
		// Reset source-specific fields when the kind changes — each variant carries
		// different config so we can't preserve fields across kinds.
		const next: TriggerNodeData['source'] =
			kind === 'cron'
				? { kind: 'cron', schedule: '0 9 * * MON-FRI', timezone: 'UTC', jitterSecs: 0 }
				: kind === 'catalog'
					? { kind: 'catalog', filters: {}, backfill: false }
					: kind === 'net_completion'
						? {
								kind: 'net_completion',
								sourceTemplateId: '00000000-0000-0000-0000-000000000000',
								on: 'success'
							}
						: kind === 'webhook'
							? { kind: 'webhook', slug: '', auth: { kind: 'none' } }
							: { kind: 'manual', form: [] };
		update('source', next);
	}

	function addMapping() {
		update('payloadMapping', [...mappings, { targetField: '', expression: 'payload' }]);
	}

	function updateMapping(idx: number, patch: Partial<FieldMapping>) {
		const next = mappings.map((m, i) => (i === idx ? { ...m, ...patch } : m));
		update('payloadMapping', next);
	}

	function removeMapping(idx: number) {
		update(
			'payloadMapping',
			mappings.filter((_, i) => i !== idx)
		);
	}
</script>

<div class="space-y-3">
	<FormField label="Source kind" for="trigger-source-kind">
		<select
			id="trigger-source-kind"
			class="w-full rounded-md border border-input bg-background px-2 py-1 text-sm"
			disabled={readonly}
			value={sourceKind}
			onchange={(e) => updateSourceKind(e.currentTarget.value as TriggerNodeData['source']['kind'])}
		>
			<option value="manual">Manual</option>
			<option value="cron">Cron schedule</option>
			<option value="catalog">Catalogue event</option>
			<option value="net_completion">Workflow completion</option>
			<option value="webhook">Webhook</option>
		</select>
	</FormField>

	<!-- Source-specific config. Phase 5a keeps it minimal — each source kind
	     gets its own editor in 5b–5e. -->
	{#if source?.kind === 'cron'}
		<FormField label="Cron schedule">
			<Input
				type="text"
				value={source.schedule}
				disabled={readonly}
				oninput={(e) =>
					update('source', { ...source, schedule: (e.currentTarget as HTMLInputElement).value })}
			/>
		</FormField>
		<FormField label="Timezone (IANA)">
			<Input
				type="text"
				value={source.timezone ?? 'UTC'}
				disabled={readonly}
				oninput={(e) =>
					update('source', { ...source, timezone: (e.currentTarget as HTMLInputElement).value })}
			/>
		</FormField>
	{:else if source?.kind === 'webhook'}
		<FormField label="Slug" for="trigger-slug">
			<Input
				id="trigger-slug"
				type="text"
				value={source.slug}
				disabled={readonly}
				placeholder="my-webhook"
				oninput={(e) =>
					update('source', { ...source, slug: (e.currentTarget as HTMLInputElement).value })}
			/>
		</FormField>
	{:else if source?.kind === 'manual'}
		<p class="text-xs text-muted-foreground">
			Manual triggers fire via <code>POST /api/triggers/{'{node_id}'}/fire</code>.
		</p>
	{/if}

	<!-- Payload mapping — each row projects one target-port field. -->
	<div class="space-y-1.5">
		<div class="flex items-center justify-between">
			<span class="text-xs font-medium text-muted-foreground">Payload mapping</span>
			{#if !readonly}
				<Button variant="ghost" size="sm" onclick={addMapping}>
					<Plus class="size-3.5" />
					Add
				</Button>
			{/if}
		</div>
		{#if mappings.length === 0}
			<p class="rounded-md border border-dashed border-border/50 p-2 text-[11px] text-muted-foreground">
				No mappings. Without entries the trigger forwards <code>payload</code> verbatim.
			</p>
		{:else}
			{#each mappings as mapping, i (i)}
				<div class="rounded-md border border-border/60 bg-muted/20 p-2 space-y-1.5">
					<div class="flex items-center gap-2">
						<Input
							type="text"
							value={mapping.targetField}
							disabled={readonly}
							placeholder="target_field"
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
						placeholder="payload.x"
						oninput={(e) =>
							updateMapping(i, {
								expression: (e.currentTarget as HTMLTextAreaElement).value
							})}
					/>
				</div>
			{/each}
		{/if}
	</div>

	<!-- Enabled toggle. -->
	<label class="flex items-center gap-2">
		<input
			type="checkbox"
			checked={enabled}
			disabled={readonly}
			onchange={(e) => update('enabled', (e.currentTarget as HTMLInputElement).checked)}
		/>
		<span class="text-sm">Enabled</span>
	</label>
	<p class="text-[11px] text-muted-foreground">
		Disabled triggers are stored with the template but the dispatcher ignores them.
	</p>
</div>
