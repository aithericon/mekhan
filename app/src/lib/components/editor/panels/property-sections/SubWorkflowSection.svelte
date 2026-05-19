<script lang="ts">
	import type { SubWorkflowNodeData } from '$lib/types/editor';
	import type { components } from '$lib/api/schema';
	import { listTemplates, type Template } from '$lib/api/client';
	import * as Select from '$lib/components/ui/select';
	import { Input } from '$lib/components/ui/input';
	import { Button } from '$lib/components/ui/button';
	import { FormField } from '$lib/components/ui/form-field';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import PortsSection from './PortsSection.svelte';

	type FieldMapping = components['schemas']['FieldMapping'];
	type Port = components['schemas']['Port'];

	type Props = {
		data: SubWorkflowNodeData;
		readonly?: boolean;
		onchange: (data: SubWorkflowNodeData) => void;
		/** The template currently being edited — excluded from the picker so a
		 *  template can't trivially call itself (the backend also rejects a
		 *  same-family self-reference at publish). */
		templateId?: string;
	};

	let { data, readonly = false, onchange, templateId }: Props = $props();

	let templates = $state<Template[]>([]);
	let loadError = $state<string | null>(null);

	// `listTemplates(published=true)` returns the latest published row per
	// family; the stable family id we persist is `base_template_id ?? id`.
	function familyId(t: Template): string {
		return t.base_template_id ?? t.id;
	}

	$effect(() => {
		let cancelled = false;
		listTemplates(1, 100, undefined, true)
			.then((res) => {
				if (cancelled) return;
				templates = (res.items ?? []).filter(
					(t) => t.id !== templateId && familyId(t) !== templateId
				);
			})
			.catch((e) => {
				if (!cancelled) loadError = String(e);
			});
		return () => {
			cancelled = true;
		};
	});

	const selectedName = $derived(
		templates.find((t) => familyId(t) === data.templateId)?.name ??
			(data.templateId ? data.templateId.slice(0, 8) : 'Select a template…')
	);

	const pinMode = $derived(data.versionPin?.mode ?? 'latest');
	const pinnedVersion = $derived(
		data.versionPin?.mode === 'pinned' ? data.versionPin.version : 1
	);

	const outputPort = $derived<Port>(
		data.output ?? { id: 'out', label: 'Result', fields: [] }
	);
	const mappings = $derived<FieldMapping[]>(data.inputMapping ?? []);

	function pickTemplate(famId: string) {
		onchange({ ...data, templateId: famId });
	}

	function setPinMode(mode: string) {
		onchange({
			...data,
			versionPin:
				mode === 'pinned' ? { mode: 'pinned', version: pinnedVersion } : { mode: 'latest' }
		});
	}

	function setPinnedVersion(v: number) {
		onchange({ ...data, versionPin: { mode: 'pinned', version: v } });
	}

	function addMapping() {
		onchange({ ...data, inputMapping: [...mappings, { targetField: '', expression: '' }] });
	}

	function updateMapping(i: number, patch: Partial<FieldMapping>) {
		const next = mappings.map((m, idx) => (idx === i ? { ...m, ...patch } : m));
		onchange({ ...data, inputMapping: next });
	}

	function removeMapping(i: number) {
		onchange({ ...data, inputMapping: mappings.filter((_, idx) => idx !== i) });
	}

	function setOutput(port: Port) {
		onchange({ ...data, output: port });
	}
</script>

<div class="space-y-4">
	<!-- Template picker -->
	<div class="space-y-1.5">
		<span class="text-sm font-medium text-muted-foreground">Child template</span>
		<Select.Root
			type="single"
			value={data.templateId}
			onValueChange={(v) => {
				if (v) pickTemplate(v);
			}}
			disabled={readonly}
		>
			<Select.Trigger disabled={readonly} data-testid="select-subworkflow-template">
				{selectedName}
			</Select.Trigger>
			<Select.Content>
				{#each templates as t (t.id)}
					<Select.Item value={familyId(t)} label={t.name} />
				{/each}
			</Select.Content>
		</Select.Root>
		{#if loadError}
			<p class="text-sm text-destructive">Could not load templates: {loadError}</p>
		{:else if templates.length === 0}
			<p class="text-sm text-muted-foreground">
				No other published templates. Publish a template first to call it here.
			</p>
		{/if}
	</div>

	<!-- Version pin -->
	<div class="space-y-1.5">
		<span class="text-sm font-medium text-muted-foreground">Version</span>
		<Select.Root
			type="single"
			value={pinMode}
			onValueChange={(v) => {
				if (v) setPinMode(v);
			}}
			disabled={readonly}
		>
			<Select.Trigger disabled={readonly} data-testid="select-subworkflow-pin">
				{pinMode === 'pinned' ? `Pinned (v${pinnedVersion})` : 'Track latest'}
			</Select.Trigger>
			<Select.Content>
				<Select.Item value="latest" label="Track latest" />
				<Select.Item value="pinned" label="Pin to a version" />
			</Select.Content>
		</Select.Root>
		{#if pinMode === 'pinned'}
			<FormField label="Pinned version" for="subworkflow-version">
				<Input
					id="subworkflow-version"
					type="number"
					min="1"
					value={pinnedVersion}
					disabled={readonly}
					data-testid="input-subworkflow-version"
					oninput={(e) =>
						setPinnedVersion(
							parseInt((e.currentTarget as HTMLInputElement).value, 10) || 1
						)}
				/>
			</FormField>
		{/if}
		<p class="text-sm text-muted-foreground">
			Resolved and frozen into this template at publish — a later child change
			won't alter an already-published parent until you re-publish.
		</p>
	</div>

	<!-- Input mapping: parent token → child Start input -->
	<div class="space-y-1.5">
		<div class="flex items-center justify-between">
			<span class="text-sm font-medium text-muted-foreground">Input mapping</span>
			{#if !readonly}
				<Button
					variant="ghost"
					size="sm"
					data-testid="btn-add-subworkflow-mapping"
					onclick={addMapping}
				>
					<Plus class="size-3.5" /> Add
				</Button>
			{/if}
		</div>
		{#if mappings.length === 0}
			<p class="text-sm text-muted-foreground">
				No mapping — the inbound token is passed to the child unchanged.
			</p>
		{/if}
		{#each mappings as m, i (i)}
			<div class="flex items-center gap-1.5">
				<Input
					class="flex-1"
					placeholder="child field"
					value={m.targetField}
					disabled={readonly}
					data-testid="input-subworkflow-map-field"
					oninput={(e) =>
						updateMapping(i, {
							targetField: (e.currentTarget as HTMLInputElement).value
						})}
				/>
				<Input
					class="flex-1"
					placeholder="Rhai expression (e.g. input.amount)"
					value={m.expression}
					disabled={readonly}
					data-testid="input-subworkflow-map-expr"
					oninput={(e) =>
						updateMapping(i, {
							expression: (e.currentTarget as HTMLInputElement).value
						})}
				/>
				{#if !readonly}
					<button
						type="button"
						class="rounded-md p-1 text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
						title="Remove mapping"
						aria-label="Remove mapping"
						onclick={() => removeMapping(i)}
					>
						<Trash2 class="size-4" />
					</button>
				{/if}
			</div>
		{/each}
	</div>

	<!-- Declared result shape, mapped back at the join -->
	<PortsSection
		port={outputPort}
		{readonly}
		title="Result"
		emptyHint="No fields declared — the child's terminal result passes through unchanged."
		onchange={setOutput}
	/>
</div>
