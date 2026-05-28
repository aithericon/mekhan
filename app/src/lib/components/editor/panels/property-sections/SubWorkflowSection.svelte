<script lang="ts">
	import type { SubWorkflowNodeData } from '$lib/types/editor';
	import type { components } from '$lib/api/schema';
	import {
		listTemplates,
		createTemplate,
		setTemplateVisibility,
		type Template
	} from '$lib/api/client';
	import * as Select from '$lib/components/ui/select';
	import { Input } from '$lib/components/ui/input';
	import { Button } from '$lib/components/ui/button';
	import { FormField } from '$lib/components/ui/form-field';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import Lock from '@lucide/svelte/icons/lock';
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
	let creating = $state(false);
	let privacyBusy = $state(false);

	// `listTemplates(published=true)` returns the latest published row per
	// family; the stable family id we persist is `base_template_id ?? id`.
	function familyId(t: Template): string {
		return t.base_template_id ?? t.id;
	}

	// The picker offers the workspace's public/shared templates PLUS this
	// workflow's own private children (hidden from the catalogue, so fetched
	// separately by owner). Private children of other workflows stay invisible.
	$effect(() => {
		let cancelled = false;
		const owner = templateId;
		Promise.all([
			listTemplates(1, 100, undefined, true),
			owner
				? listTemplates(1, 100, undefined, true, undefined, undefined, owner)
				: Promise.resolve({ items: [] as Template[] })
		])
			.then(([shared, mine]) => {
				if (cancelled) return;
				const byFamily = new Map<string, Template>();
				for (const t of [...(shared.items ?? []), ...(mine.items ?? [])]) {
					if (t.id === templateId || familyId(t) === templateId) continue;
					byFamily.set(familyId(t), t);
				}
				templates = [...byFamily.values()];
			})
			.catch((e) => {
				if (!cancelled) loadError = String(e);
			});
		return () => {
			cancelled = true;
		};
	});

	const selectedTemplate = $derived(
		templates.find((t) => familyId(t) === data.templateId)
	);
	const selectedIsPrivate = $derived(selectedTemplate?.visibility === 'private');

	const selectedName = $derived(
		selectedTemplate?.name ??
			(data.templateId ? data.templateId.slice(0, 8) : 'Select a template…')
	);

	// Create a blank child template bound private to THIS workflow, point the
	// node at it, and open it for editing in a new tab. New-tab (not goto)
	// because the Yjs editor session is pinned at mount — cross-template
	// editing needs a fresh page. The author publishes the child from its own
	// tab before publishing this parent.
	async function createPrivateChild() {
		if (creating || !templateId) return;
		creating = true;
		loadError = null;
		try {
			const child = await createTemplate({ name: 'Untitled sub-workflow', description: '' });
			await setTemplateVisibility(child.id, 'private', templateId);
			pickTemplate(familyId(child));
			templates = [
				...templates,
				{ ...child, visibility: 'private', owner_template_id: templateId }
			];
			window.open(`/templates/${child.id}`, '_blank');
		} catch (e) {
			loadError = String(e);
		} finally {
			creating = false;
		}
	}

	// Retroactively scope an already-selected child to this workflow.
	async function makePrivateToThisWorkflow() {
		const fam = data.templateId;
		if (!fam || !templateId || privacyBusy) return;
		privacyBusy = true;
		loadError = null;
		try {
			await setTemplateVisibility(fam, 'private', templateId);
			templates = templates.map((t) =>
				familyId(t) === fam
					? { ...t, visibility: 'private', owner_template_id: templateId }
					: t
			);
		} catch (e) {
			loadError = String(e);
		} finally {
			privacyBusy = false;
		}
	}

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

		{#if !readonly && templateId}
			<div class="space-y-1.5 pt-1">
				{#if data.templateId && selectedIsPrivate}
					<span
						class="flex items-center gap-1.5 text-sm text-muted-foreground"
						data-testid="subworkflow-private-badge"
					>
						<Lock class="size-4" />
						Private to this workflow
					</span>
				{:else if data.templateId}
					<Button
						variant="ghost"
						size="sm"
						class="w-full justify-start"
						onclick={makePrivateToThisWorkflow}
						disabled={privacyBusy}
						data-testid="btn-make-subworkflow-private"
					>
						<Lock class="size-4" />
						{privacyBusy ? 'Making private…' : 'Make private to this workflow'}
					</Button>
				{/if}
				<Button
					variant="outline"
					size="sm"
					class="w-full justify-start"
					onclick={createPrivateChild}
					disabled={creating}
					data-testid="btn-create-private-subworkflow"
				>
					<Plus class="size-4" />
					{creating ? 'Creating…' : 'Create private sub-workflow'}
				</Button>
			</div>
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
