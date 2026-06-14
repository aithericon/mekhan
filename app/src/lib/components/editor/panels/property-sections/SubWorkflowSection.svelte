<script lang="ts">
	import type { SubWorkflowNodeData } from '$lib/types/editor';
	import type { components } from '$lib/api/schema';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import {
		listTemplates,
		createTemplate,
		setTemplateVisibility,
		getTemplateIoContract,
		getUpgradePreview,
		type Template,
		type UpgradePreview
	} from '$lib/api/client';
	import { untrack } from 'svelte';
	import { portsEqual } from '$lib/editor/port-utils';
	import { familyId } from '$lib/editor/template-utils';
	import { createDebouncedFetcher } from '$lib/editor/debounced-fetcher';
	import * as Select from '$lib/components/ui/select';
	import { Input } from '$lib/components/ui/input';
	import { Button } from '$lib/components/ui/button';
	import { FormField } from '$lib/components/ui/form-field';
	import Plus from '@lucide/svelte/icons/plus';
	import Lock from '@lucide/svelte/icons/lock';
	import ArrowUpCircle from '@lucide/svelte/icons/arrow-up-circle';
	import TriangleAlert from '@lucide/svelte/icons/triangle-alert';
	import ChevronsUpDown from '@lucide/svelte/icons/chevrons-up-down';
	import ExternalLink from '@lucide/svelte/icons/external-link';
	import DerivedPortsSection from './DerivedPortsSection.svelte';
	import OutputSchemaSection from './OutputSchemaSection.svelte';
	import RefPicker from './RefPicker.svelte';
	import ChildWorkflowBrowser from '$lib/components/editor/ChildWorkflowBrowser.svelte';
	import { portToSchemaNode } from '$lib/schema/model';

	type FieldMapping = components['schemas']['FieldMapping'];
	type Port = components['schemas']['Port'];
	type PortField = components['schemas']['PortField'];

	type Props = {
		data: SubWorkflowNodeData;
		readonly?: boolean;
		onchange: (data: SubWorkflowNodeData) => void;
		/** The template currently being edited — excluded from the picker so a
		 *  template can't trivially call itself (the backend also rejects a
		 *  same-family self-reference at publish). */
		templateId?: string;
		/** In-scope refs for the input-mapping RefPickers. */
		scope?: ScopeEntry[];
	};

	let { data, readonly = false, onchange, templateId, scope = [] }: Props = $props();

	let templates = $state<Template[]>([]);
	let loadError = $state<string | null>(null);
	let creating = $state(false);
	let privacyBusy = $state(false);
	let browserOpen = $state(false);

	// The child's derived contract: input fields (from its Start `initial`
	// port) drive the fixed mapping rows; output (union of its End
	// `result_mapping`) is shown read-only and persisted onto `data.output` so
	// the borrow resolver / variable picker see the child's true return shape.
	let inputFields = $state<PortField[]>([]);
	let contractError = $state<string | null>(null);

	// The picker offers the workspace's public/shared templates PLUS this
	// workflow's own private children (hidden from the catalogue, so fetched
	// separately by owner). Private children of other workflows stay invisible.
	$effect(() => {
		let cancelled = false;
		const owner = templateId;
		Promise.all([
			listTemplates({ pageSize: 100, published: true }),
			owner
				? listTemplates({ pageSize: 100, ownerTemplateId: owner })
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

	// Fetch the child's derived io-contract whenever the picked template or
	// version pin changes, and reconcile it onto the node:
	//   - persist `data.output` (read-only) so the variable picker / borrow
	//     resolver surface the child's result fields,
	//   - drive the fixed input rows from `input.fields`,
	//   - prune any input_mapping rows whose target field no longer exists in
	//     the child's Start contract.
	// Debounced + seq-guarded (mirrors AutomatedStepSection's derived effect)
	// so a quick re-pin doesn't apply a stale contract. Server-authoritative:
	// the editor never derives locally, so this preview can't drift from what
	// publish freezes. On fetch failure we surface the error and leave the
	// existing mapping/output untouched (no destructive prune on transients).
	const contractFetcher = createDebouncedFetcher();
	$effect(() => {
		const fam = data.templateId;
		const version = data.versionPin?.mode === 'pinned' ? data.versionPin.version : undefined;
		if (!fam) {
			inputFields = [];
			contractError = null;
			return;
		}
		contractFetcher.schedule(async (fresh) => {
			try {
				const c = await getTemplateIoContract(fam, version);
				if (!fresh()) return;
				contractError = null;
				untrack(() => {
					inputFields = c.input.fields ?? [];
					const patch: Partial<SubWorkflowNodeData> = {};
					if (!portsEqual(data.output, c.output)) {
						patch.output = c.output;
					}
					// Snapshot the child's input contract onto the node so the
					// canvas can show "what this sub-workflow consumes" (the way a
					// Start node shows its fields) without the panel open —
					// symmetric with the `output` snapshot above. Display-only:
					// publish re-derives the real input from the frozen child.
					if (!portsEqual(data.inputContract, c.input)) {
						patch.inputContract = c.input;
					}
					// Freeze the child's library-node branding (decision 12) onto the
					// node, symmetric with the contract snapshot above. Clears when
					// re-pointed at a non-library template (c.* undefined). Display
					// only — never feeds compilation.
					const nextPresentation = c.presentation ?? undefined;
					const nextCoordinate = c.coordinate ?? undefined;
					if (
						JSON.stringify(data.presentation ?? null) !==
						JSON.stringify(nextPresentation ?? null)
					) {
						patch.presentation = nextPresentation;
					}
					if ((data.sourceCoordinate ?? undefined) !== nextCoordinate) {
						patch.sourceCoordinate = nextCoordinate;
					}
					if (!readonly) {
						const valid = new Set(inputFields.map((f) => f.name));
						const pruned = mappings.filter((m) => valid.has(m.targetField));
						if (pruned.length !== mappings.length) {
							patch.inputMapping = pruned;
						}
					}
					if (Object.keys(patch).length > 0) {
						onchange({ ...data, ...patch });
					}
				});
			} catch (e) {
				if (!fresh()) return;
				contractError = String(e);
				inputFields = [];
			}
		});
	});

	// Upgrade detection (Phase 5): for a PINNED library-node embed, ask the
	// backend whether a newer family version exists and how its contract differs.
	// `latest`-tracking embeds never need a prompt — they always resolve newest.
	// Debounced + seq-guarded like the contract fetch so a quick re-pin can't
	// apply a stale verdict.
	let upgrade = $state<UpgradePreview | null>(null);
	const upgradeFetcher = createDebouncedFetcher();
	$effect(() => {
		const coordinate = data.sourceCoordinate;
		const from = data.versionPin?.mode === 'pinned' ? data.versionPin.version : undefined;
		if (!coordinate || from === undefined) {
			upgrade = null;
			return;
		}
		upgradeFetcher.schedule(async (fresh) => {
			try {
				const p = await getUpgradePreview(coordinate, from);
				if (!fresh()) return;
				upgrade = p.classification === 'up_to_date' ? null : p;
			} catch {
				if (!fresh()) return;
				upgrade = null; // a missing/unreadable family just means "no prompt"
			}
		});
	});

	// Input fields whose mapping a breaking upgrade would disturb (removed /
	// retyped / newly-required). Drives the per-row "needs remap" badge.
	const affectedFields = $derived(new Set(upgrade?.affectedInputFields ?? []));

	// Adopt the offered version: re-pin to it. The contract effect then refetches
	// the new child contract, reconciles `data.output`/`inputContract`, and prunes
	// any input-mapping row whose target field no longer exists — so the author
	// finishes the remap in the SAME fixed-row editor below.
	function adoptUpgrade() {
		if (!upgrade) return;
		setPinnedVersion(upgrade.toVersion);
	}

	function pickTemplate(famId: string) {
		onchange({ ...data, templateId: famId });
	}

	// Open the selected child's editor in a new tab. New-tab (not goto) because
	// the Yjs editor session is pinned at mount. Prefer the resolved latest row
	// id; fall back to the family id (its v1 row) if not yet loaded.
	function openSelectedInTab() {
		const rowId = selectedTemplate?.id ?? data.templateId;
		if (rowId) window.open(`/templates/${rowId}`, '_blank');
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

	// Input wiring is fixed to the child's Start fields: the target field is
	// locked, only the expression is authored. An empty expression drops the
	// row (that child field falls through unmapped); a non-empty one upserts it.
	function exprFor(fieldName: string): string {
		return mappings.find((m) => m.targetField === fieldName)?.expression ?? '';
	}

	function setExpr(fieldName: string, expression: string) {
		let next: FieldMapping[];
		if (expression.trim() === '') {
			next = mappings.filter((m) => m.targetField !== fieldName);
		} else if (mappings.some((m) => m.targetField === fieldName)) {
			next = mappings.map((m) => (m.targetField === fieldName ? { ...m, expression } : m));
		} else {
			next = [...mappings, { targetField: fieldName, expression }];
		}
		onchange({ ...data, inputMapping: next });
	}
</script>

<div class="space-y-4">
	<!-- Template picker -->
	<div class="space-y-1.5">
		<span class="text-sm font-medium text-muted-foreground">Child template</span>
		<div class="flex items-center gap-1.5">
			<Button
				variant="outline"
				class="min-w-0 flex-1 justify-between font-normal"
				disabled={readonly}
				onclick={() => (browserOpen = true)}
				data-testid="btn-open-subworkflow-browser"
			>
				<span class="truncate">{selectedName}</span>
				<ChevronsUpDown class="size-4 shrink-0 opacity-50" />
			</Button>
			{#if data.templateId}
				<Button
					variant="ghost"
					size="icon"
					title="Open child workflow in a new tab"
					onclick={openSelectedInTab}
					data-testid="btn-open-subworkflow-child"
				>
					<ExternalLink class="size-4" />
				</Button>
			{/if}
		</div>
		{#if loadError}
			<p class="text-sm text-destructive">Could not load templates: {loadError}</p>
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

	<!-- Upgrade prompt (Phase 5): a newer version of this library node exists.
	     Compatible = drop-in; breaking = some inputs need remapping after adopt. -->
	{#if upgrade}
		{@const breaking = upgrade.classification === 'breaking'}
		<div
			class="space-y-2 rounded-md border p-2 {breaking
				? 'border-amber-500/50 bg-amber-500/5'
				: 'border-primary/40 bg-primary/5'}"
			data-testid="subworkflow-upgrade"
		>
			<div class="flex items-center gap-1.5 text-sm font-medium">
				{#if breaking}
					<TriangleAlert class="size-4 text-amber-500" />
					<span data-testid="upgrade-classification"
						>v{upgrade.toVersion} available — breaking</span
					>
				{:else}
					<ArrowUpCircle class="size-4 text-primary" />
					<span data-testid="upgrade-classification"
						>v{upgrade.toVersion} available — compatible</span
					>
				{/if}
			</div>
			<p class="text-sm text-muted-foreground">
				You're pinned to v{upgrade.fromVersion}.
				{#if breaking}
					Adopting it changes the input contract; the highlighted fields below need
					remapping.
				{:else}
					It's a drop-in upgrade — no input changes.
				{/if}
			</p>
			{#if breaking && upgrade.affectedInputFields.length > 0}
				<ul class="space-y-0.5 text-sm text-muted-foreground">
					{#each upgrade.affectedInputFields as f (f)}
						<li class="font-mono">• {f}</li>
					{/each}
				</ul>
			{/if}
			<Button
				variant={breaking ? 'outline' : 'default'}
				size="sm"
				class="w-full"
				disabled={readonly}
				onclick={adoptUpgrade}
				data-testid="btn-upgrade-subworkflow"
			>
				<ArrowUpCircle class="size-4" />
				Upgrade to v{upgrade.toVersion}
			</Button>
		</div>
	{/if}

	{#if contractError}
		<p class="rounded-md border border-dashed border-destructive/40 p-2 text-sm text-destructive">
			Couldn't read the child's contract: {contractError}. Publish the child template,
			then reopen this panel.
		</p>
	{/if}

	<!-- Input mapping: fixed to the child's Start fields. The target field is
	     locked; only the expression (a Rhai borrow) is authored. -->
	<div class="space-y-1.5">
		<span class="text-sm font-medium text-muted-foreground">Input mapping</span>
		{#if !data.templateId}
			<p class="text-sm text-muted-foreground">Pick a child template to map its inputs.</p>
		{:else if inputFields.length === 0}
			<p class="text-sm text-muted-foreground">
				The child declares no Start fields — the inbound token is passed through
				unchanged.
			</p>
		{:else}
			{#each inputFields as field (field.name)}
				<div class="space-y-1 rounded-md border border-border/60 bg-muted/20 p-2">
					<div class="flex items-center justify-between">
						<span
							class="font-mono text-sm text-foreground"
							data-testid="subworkflow-input-field"
						>
							{field.name}
						</span>
						<span class="text-sm text-muted-foreground">
							{#if affectedFields.has(field.name)}
								<span class="font-medium text-amber-600" data-testid="subworkflow-field-remap"
									>needs remap • </span
								>
							{/if}
							{field.kind}{field.required ? ' • required' : ''}
						</span>
					</div>
					<RefPicker
						{scope}
						disabled={readonly}
						selected={exprFor(field.name) || undefined}
						placeholder="Pick source field…"
						onpick={(entry) => setExpr(field.name, entry.qualified)}
					/>
					<Input
						class="font-mono"
						placeholder="Rhai expression (e.g. input.amount)"
						value={exprFor(field.name)}
						disabled={readonly}
						data-testid="input-subworkflow-map-expr"
						oninput={(e) => setExpr(field.name, (e.currentTarget as HTMLInputElement).value)}
					/>
				</div>
			{/each}
		{/if}
	</div>

	<!-- Result: derived from the child's End result mapping, read-only. -->
	<DerivedPortsSection ports={[outputPort]} title="Result" derivedFrom="Child End" />

	<!-- Output schema: expandable type tree for the child's result port. -->
	{#if outputPort.fields && outputPort.fields.length > 0}
		<OutputSchemaSection node={portToSchemaNode(outputPort)} title="Output schema" />
	{/if}

	<!-- Input contract: expandable type tree for the child's Start fields. -->
	{#if data.inputContract && (data.inputContract.fields?.length ?? 0) > 0}
		<OutputSchemaSection node={portToSchemaNode(data.inputContract)} title="Input contract" />
	{/if}
</div>

<ChildWorkflowBrowser
	bind:open={browserOpen}
	currentTemplateId={templateId}
	onselect={(famId) => pickTemplate(famId)}
/>
