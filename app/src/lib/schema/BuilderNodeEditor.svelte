<script lang="ts">
	/**
	 * Recursive, BuilderNode-native editor body.
	 *
	 * This component renders ALL per-kind editor UI (object/array/scalar/union/
	 * ref/raw) and recurses by importing ITSELF for nested editors — passing the
	 * child `BuilderNode` DIRECTLY (no builderNodeToSchema/schemaToBuilderNode
	 * round-trip between levels). The round-trip between levels was the source of
	 * both the focus-loss-on-edit bug and round-trip corruption; passing nodes by
	 * reference keeps identity stable.
	 *
	 * The depth-0 root-kind switcher and the raw-JSON escape hatch are owned by
	 * the thin SchemaBuilder.svelte wrapper, NOT here.
	 */
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import GripVertical from '@lucide/svelte/icons/grip-vertical';
	import AlertCircle from '@lucide/svelte/icons/alert-circle';
	import Link from '@lucide/svelte/icons/link';
	import GitMerge from '@lucide/svelte/icons/git-merge';
	import { Input } from '$lib/components/ui/input';
	import { Label } from '$lib/components/ui/label';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import * as Select from '$lib/components/ui/select';
	import SchemaView from './SchemaView.svelte';
	import { jsonSchemaToSchemaNode } from './model';
	// Self-import for recursion (avoids deprecated <svelte:self>).
	import BuilderNodeEditor from './BuilderNodeEditor.svelte';
	import {
		nextFieldId,
		uniqueFieldName,
		slugifyFieldName,
		type BuilderNode,
		type BuilderObjectNode,
		type BuilderArrayNode,
		type BuilderScalarNode,
		type BuilderUnionNode,
		type FieldKindHint,
		type ScalarJsonType
	} from './builder-model';

	// ── Props ────────────────────────────────────────────────────────────────

	type Props = {
		node: BuilderNode;
		/** Propagate an edited node back to the parent. */
		onNodeChange: (n: BuilderNode) => void;
		/** When true the root node may be a scalar (passed down to nested editors). */
		allowRootScalar?: boolean;
		/** Workflow-level reusable schema definitions (name → schema). */
		definitions?: Record<string, unknown>;
		/** When true, all inputs are disabled and structural mutations are hidden. */
		readonly?: boolean;
		/** Internal: nesting depth, drives indentation & compact rendering. */
		_depth?: number;
		/**
		 * Optional escape to raw-JSON editing, owned by the root wrapper. When
		 * absent the raw-only notice hides its "Edit as raw JSON" button.
		 */
		onEditRaw?: () => void;
	};

	let {
		node,
		onNodeChange,
		allowRootScalar = false,
		definitions = {},
		readonly = false,
		_depth = 0,
		onEditRaw
	}: Props = $props();

	const definitionNames = $derived(Object.keys(definitions));

	// ── Object-node helpers ───────────────────────────────────────────────────

	function addField() {
		if (node.kind !== 'object') return;
		const names = node.fields.map((f) => f.name);
		const name = uniqueFieldName(names);
		onNodeChange({
			...node,
			fields: [
				...node.fields,
				{
					id: nextFieldId(),
					name,
					node: { kind: 'scalar', type: 'string', nullable: false, enumValues: [] }
				}
			]
		});
	}

	function removeField(idx: number) {
		if (node.kind !== 'object') return;
		const fields = node.fields.filter((_, i) => i !== idx);
		// Remove from required if present.
		const removedName = node.fields[idx]?.name;
		const required = new Set(node.required);
		if (removedName) required.delete(removedName);
		onNodeChange({ ...node, fields, required });
	}

	function renameField(idx: number, rawName: string) {
		if (node.kind !== 'object') return;
		const newName = slugifyFieldName(rawName);
		const oldName = node.fields[idx]?.name;
		const fields = node.fields.map((f, i) => (i === idx ? { ...f, name: newName } : f));
		const required = new Set(node.required);
		if (oldName && required.has(oldName)) {
			required.delete(oldName);
			if (newName) required.add(newName);
		}
		onNodeChange({ ...node, fields, required });
	}

	function updateFieldNode(idx: number, childNode: BuilderNode) {
		if (node.kind !== 'object') return;
		const fields = node.fields.map((f, i) => (i === idx ? { ...f, node: childNode } : f));
		onNodeChange({ ...node, fields });
	}

	function toggleRequired(fieldName: string, checked: boolean) {
		if (node.kind !== 'object') return;
		const required = new Set(node.required);
		if (checked) required.add(fieldName);
		else required.delete(fieldName);
		onNodeChange({ ...node, required });
	}

	function moveField(from: number, to: number) {
		if (node.kind !== 'object') return;
		const fields = [...node.fields];
		const [item] = fields.splice(from, 1);
		fields.splice(to, 0, item);
		onNodeChange({ ...node, fields });
	}

	function setObjectProp<K extends keyof BuilderObjectNode>(key: K, value: BuilderObjectNode[K]) {
		if (node.kind !== 'object') return;
		onNodeChange({ ...node, [key]: value });
	}

	// ── Array-node helpers ────────────────────────────────────────────────────

	function setArrayItems(itemsNode: BuilderNode) {
		if (node.kind !== 'array') return;
		onNodeChange({ ...node, items: itemsNode });
	}

	function setArrayProp<K extends keyof BuilderArrayNode>(key: K, value: BuilderArrayNode[K]) {
		if (node.kind !== 'array') return;
		onNodeChange({ ...node, [key]: value });
	}

	// ── Scalar-node helpers ───────────────────────────────────────────────────

	function setScalarProp<K extends keyof BuilderScalarNode>(key: K, value: BuilderScalarNode[K]) {
		if (node.kind !== 'scalar') return;
		onNodeChange({ ...node, [key]: value });
	}

	function setScalarType(type: ScalarJsonType) {
		if (node.kind !== 'scalar') return;
		// Clear type-incompatible constraints when switching types.
		const next: BuilderScalarNode = {
			...node,
			type,
			enumValues: [],
			minimum: undefined,
			maximum: undefined,
			minLength: undefined,
			maxLength: undefined,
			pattern: undefined,
			format: undefined
		};
		onNodeChange(next);
	}

	// ── Enum helpers ──────────────────────────────────────────────────────────

	function addEnumValue() {
		if (node.kind !== 'scalar') return;
		onNodeChange({ ...node, enumValues: [...node.enumValues, ''] });
	}

	function updateEnumValue(idx: number, value: string) {
		if (node.kind !== 'scalar') return;
		const enumValues = node.enumValues.map((v, i) => (i === idx ? value : v));
		onNodeChange({ ...node, enumValues });
	}

	function removeEnumValue(idx: number) {
		if (node.kind !== 'scalar') return;
		const enumValues = node.enumValues.filter((_, i) => i !== idx);
		onNodeChange({ ...node, enumValues });
	}

	// ── Union-node helpers ────────────────────────────────────────────────────

	function setUnionProp<K extends keyof BuilderUnionNode>(key: K, value: BuilderUnionNode[K]) {
		if (node.kind !== 'union') return;
		onNodeChange({ ...node, [key]: value });
	}

	function addUnionVariant() {
		if (node.kind !== 'union') return;
		onNodeChange({
			...node,
			variants: [
				...node.variants,
				{ kind: 'object', fields: [], required: new Set(), sealed: false, nullable: false }
			]
		});
	}

	function removeUnionVariant(idx: number) {
		if (node.kind !== 'union') return;
		const variants = node.variants.filter((_, i) => i !== idx);
		onNodeChange({ ...node, variants });
	}

	function updateUnionVariant(idx: number, childNode: BuilderNode) {
		if (node.kind !== 'union') return;
		const variants = node.variants.map((v, i) => (i === idx ? childNode : v));
		onNodeChange({ ...node, variants });
	}

	// Get the discriminator tag value for a variant (the const-enum value of the
	// discriminator property) so the UI can label variants meaningfully.
	function variantTagLabel(variantNode: BuilderNode, discriminator: string): string | null {
		if (variantNode.kind !== 'object') return null;
		const field = variantNode.fields.find((f) => f.name === discriminator);
		if (!field) return null;
		const fn = field.node;
		if (fn.kind === 'scalar' && fn.enumValues.length === 1) {
			return fn.enumValues[0];
		}
		return null;
	}

	// ── Ref-node helpers ──────────────────────────────────────────────────────

	function setRefName(name: string) {
		if (node.kind !== 'ref') return;
		onNodeChange({ ...node, name });
	}

	// Resolve the referenced definition for preview.
	const resolvedRefNode = $derived.by(() => {
		if (node.kind !== 'ref' || !node.name) return null;
		const defSchema = definitions[node.name];
		if (defSchema === undefined) return null;
		return jsonSchemaToSchemaNode(defSchema, definitions);
	});

	// ── Per-row field-name drafts ─────────────────────────────────────────────
	// The name <input> binds to a LOCAL draft keyed by field.id so the user can
	// type capitals/spaces/multi-char names naturally; slugify is deferred to
	// blur / Enter. Seeded lazily from field.name.

	let nameDrafts = $state<Map<string, string>>(new Map());

	function fieldNameDraft(id: string, current: string): string {
		const d = nameDrafts.get(id);
		return d !== undefined ? d : current;
	}

	function setNameDraft(id: string, value: string) {
		const next = new Map(nameDrafts);
		next.set(id, value);
		nameDrafts = next;
	}

	function commitNameDraft(idx: number, id: string) {
		const draft = nameDrafts.get(id);
		if (draft !== undefined) {
			renameField(idx, draft);
			const next = new Map(nameDrafts);
			next.delete(id);
			nameDrafts = next;
		}
	}

	// ── Expanded-section state ────────────────────────────────────────────────

	/** Which fields are expanded to show their child builder — keyed by field.id. */
	let expandedFields = $state<Set<string>>(new Set());

	function toggleFieldExpanded(id: string) {
		const next = new Set(expandedFields);
		if (next.has(id)) next.delete(id);
		else next.add(id);
		expandedFields = next;
	}

	/** Which union variants are expanded. */
	let expandedVariants = $state<Set<number>>(new Set([0]));

	function toggleVariantExpanded(idx: number) {
		const next = new Set(expandedVariants);
		if (next.has(idx)) next.delete(idx);
		else next.add(idx);
		expandedVariants = next;
	}

	/** Whether the meta/constraints section is expanded. */
	let metaExpanded = $state(false);

	// ── Type vocabulary ───────────────────────────────────────────────────────

	const SCALAR_TYPES: { value: ScalarJsonType; label: string }[] = [
		{ value: 'string', label: 'String' },
		{ value: 'number', label: 'Number' },
		{ value: 'integer', label: 'Integer' },
		{ value: 'boolean', label: 'Boolean' }
	];

	const FIELD_KIND_HINTS: { value: FieldKindHint; label: string }[] = [
		{ value: 'text', label: 'Text' },
		{ value: 'textarea', label: 'Long text' },
		{ value: 'number', label: 'Number' },
		{ value: 'bool', label: 'Boolean' },
		{ value: 'select', label: 'Select' },
		{ value: 'file', label: 'File' },
		{ value: 'signature', label: 'Signature' },
		{ value: 'timestamp', label: 'Timestamp' },
		{ value: 'json', label: 'JSON' }
	];

	// ── Drag state for field reordering ──────────────────────────────────────

	let dragIndex = $state<number | null>(null);
	let dragOverIndex = $state<number | null>(null);

	function onDragStart(idx: number, e: DragEvent) {
		dragIndex = idx;
		if (e.dataTransfer) {
			e.dataTransfer.effectAllowed = 'move';
		}
	}

	function onDragOver(idx: number, e: DragEvent) {
		e.preventDefault();
		if (e.dataTransfer) e.dataTransfer.dropEffect = 'move';
		dragOverIndex = idx;
	}

	function onDrop(toIdx: number) {
		if (dragIndex !== null && dragIndex !== toIdx) {
			moveField(dragIndex, toIdx);
		}
		dragIndex = null;
		dragOverIndex = null;
	}

	function onDragEnd() {
		dragIndex = null;
		dragOverIndex = null;
	}

	// ── Numeric input helper ─────────────────────────────────────────────────

	function parseOptionalNumber(s: string): number | undefined {
		if (s === '') return undefined;
		const n = Number(s);
		return isNaN(n) ? undefined : n;
	}
</script>

<div class="min-w-0 space-y-2">
	<!-- ── Raw-only node (unsupported construct) ─────────────────────── -->
	{#if node.kind === 'raw'}
		<div
			class="flex items-start gap-2 rounded-md border border-amber-200 bg-amber-50 px-3 py-2.5 dark:border-amber-800/40 dark:bg-amber-900/10"
			data-testid="schema-builder-raw-only"
		>
			<AlertCircle class="mt-0.5 size-4 shrink-0 text-amber-600 dark:text-amber-400" />
			<div class="min-w-0 space-y-1">
				<p class="text-sm text-amber-800 dark:text-amber-300">{node.reason}</p>
				{#if onEditRaw}
					<button
						type="button"
						class="text-xs text-amber-700 underline-offset-2 hover:underline dark:text-amber-400"
						onclick={onEditRaw}
					>
						Edit as raw JSON
					</button>
				{/if}
			</div>
		</div>

		<!-- ── Object node ───────────────────────────────────────────────── -->
	{:else if node.kind === 'object'}
		<div class="space-y-2">
			<!-- Object meta (title) — depth 0 only -->
			{#if _depth === 0}
				<div class="space-y-1.5">
					<Label class="text-xs text-muted-foreground">Title (optional)</Label>
					<Input
						type="text"
						value={node.title ?? ''}
						placeholder="e.g. Result"
						disabled={readonly}
						oninput={(e) =>
							setObjectProp('title', (e.currentTarget as HTMLInputElement).value || undefined)}
					/>
				</div>
			{/if}

			<!-- Fields list -->
			<div class="space-y-1.5">
				{#if node.fields.length > 0}
					<div class="space-y-1" role="list" data-testid="schema-builder-fields">
						{#each node.fields as field, idx (field.id)}
							{@const isExpanded = expandedFields.has(field.id)}
							{@const isDraggingOver = dragOverIndex === idx && dragIndex !== idx}
							<div
								role="listitem"
								class="rounded-md border transition-colors {isDraggingOver
									? 'border-primary/50 bg-primary/5'
									: 'border-border/60 bg-background'}"
								draggable={!readonly}
								ondragstart={(e) => onDragStart(idx, e)}
								ondragover={(e) => onDragOver(idx, e)}
								ondrop={() => onDrop(idx)}
								ondragend={onDragEnd}
								data-testid="schema-builder-field-{idx}"
							>
								<!-- Field header row -->
								<div class="flex items-center gap-1.5 p-2">
									<!-- Drag handle -->
									{#if !readonly}
										<span
											class="cursor-grab text-muted-foreground/40 hover:text-muted-foreground active:cursor-grabbing"
										>
											<GripVertical class="size-3.5 shrink-0" />
										</span>
									{/if}

									<!-- Expand toggle for child builder -->
									<button
										type="button"
										class="shrink-0 rounded p-0.5 text-muted-foreground transition-colors hover:text-foreground"
										onclick={() => toggleFieldExpanded(field.id)}
										aria-label={isExpanded ? 'Collapse field' : 'Expand field'}
										aria-expanded={isExpanded}
									>
										{#if isExpanded}
											<ChevronDown class="size-3.5" />
										{:else}
											<ChevronRight class="size-3.5" />
										{/if}
									</button>

									<!-- Field name input — local draft, slugify on blur/Enter -->
									<Input
										type="text"
										value={fieldNameDraft(field.id, field.name)}
										placeholder="field_name"
										class="flex-1 font-mono text-xs h-7"
										disabled={readonly}
										oninput={(e) =>
											setNameDraft(field.id, (e.currentTarget as HTMLInputElement).value)}
										onblur={() => commitNameDraft(idx, field.id)}
										onkeydown={(e) => {
											if (e.key === 'Enter') {
												e.preventDefault();
												commitNameDraft(idx, field.id);
											}
										}}
										data-testid="schema-builder-field-name-{idx}"
									/>

									<!-- Field type badge / selector -->
									<div class="shrink-0">
										{#if field.node.kind === 'raw'}
											<span
												class="rounded bg-amber-100 px-1.5 py-0.5 text-xs text-amber-700 dark:bg-amber-900/30 dark:text-amber-400"
												>raw</span
											>
										{:else if field.node.kind === 'object'}
											<span
												class="rounded bg-slate-100 px-1.5 py-0.5 text-xs text-slate-600 dark:bg-slate-800 dark:text-slate-300"
												>object</span
											>
										{:else if field.node.kind === 'array'}
											<span
												class="rounded bg-indigo-100 px-1.5 py-0.5 text-xs text-indigo-700 dark:bg-indigo-900/30 dark:text-indigo-300"
												>array</span
											>
										{:else if field.node.kind === 'union'}
											<span
												class="rounded bg-violet-100 px-1.5 py-0.5 text-xs text-violet-700 dark:bg-violet-900/30 dark:text-violet-300"
												>{field.node.combinator}</span
											>
										{:else if field.node.kind === 'ref'}
											<span
												class="rounded bg-teal-100 px-1.5 py-0.5 font-mono text-xs text-teal-700 dark:bg-teal-900/30 dark:text-teal-300"
												>$ref</span
											>
										{:else if field.node.kind === 'scalar'}
											<span
												class="rounded bg-blue-100 px-1.5 py-0.5 font-mono text-xs text-blue-700 dark:bg-blue-900/30 dark:text-blue-300"
												>{field.node.type}</span
											>
										{/if}
									</div>

									<!-- Required toggle -->
									<label class="flex shrink-0 items-center gap-1">
										<Checkbox
											checked={node.required.has(field.name)}
											disabled={readonly}
											onCheckedChange={(v) => toggleRequired(field.name, v === true)}
										/>
										<span class="text-xs text-muted-foreground">req</span>
									</label>

									<!-- Remove button -->
									{#if !readonly}
										<button
											type="button"
											class="shrink-0 rounded p-0.5 text-muted-foreground transition-colors hover:text-destructive"
											onclick={() => removeField(idx)}
											aria-label="Remove field"
											data-testid="schema-builder-field-remove-{idx}"
										>
											<Trash2 class="size-3.5" />
										</button>
									{/if}
								</div>

								<!-- Expanded child builder — pass the BuilderNode directly -->
								{#if isExpanded}
									<div class="border-t border-border/50 p-2 pl-8">
										<BuilderNodeEditor
											node={field.node}
											onNodeChange={(n) => updateFieldNode(idx, n)}
											allowRootScalar={true}
											{definitions}
											{readonly}
											_depth={_depth + 1}
											{onEditRaw}
										/>
									</div>
								{/if}
							</div>
						{/each}
					</div>
				{/if}

				<!-- Add field button -->
				{#if !readonly}
					<button
						type="button"
						class="flex w-full items-center justify-center gap-1.5 rounded-md border border-dashed border-border py-2 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
						onclick={addField}
						data-testid="schema-builder-add-field"
					>
						<Plus class="size-4" />
						Add field
					</button>
				{/if}
			</div>

			<!-- Object constraints / meta toggle -->
			<div class="rounded-md border border-border/40">
				<button
					type="button"
					class="flex w-full items-center justify-between px-3 py-2 text-xs text-muted-foreground hover:text-foreground"
					onclick={() => (metaExpanded = !metaExpanded)}
				>
					<span>Options &amp; constraints</span>
					{#if metaExpanded}
						<ChevronDown class="size-3.5" />
					{:else}
						<ChevronRight class="size-3.5" />
					{/if}
				</button>
				{#if metaExpanded}
					<div class="space-y-2.5 border-t border-border/40 px-3 pb-3 pt-2">
						<div class="space-y-1">
							<Label class="text-xs text-muted-foreground">Description (optional)</Label>
							<Input
								type="text"
								value={node.description ?? ''}
								placeholder="What this object represents…"
								disabled={readonly}
								oninput={(e) =>
									setObjectProp(
										'description',
										(e.currentTarget as HTMLInputElement).value || undefined
									)}
							/>
						</div>
						<label class="flex items-center gap-2">
							<Checkbox
								checked={node.nullable}
								disabled={readonly}
								onCheckedChange={(v) => setObjectProp('nullable', v === true)}
							/>
							<span class="text-xs text-muted-foreground">Nullable (type: ["object", "null"])</span>
						</label>
						<label class="flex items-center gap-2">
							<Checkbox
								checked={node.sealed}
								disabled={readonly}
								onCheckedChange={(v) => setObjectProp('sealed', v === true)}
							/>
							<span class="text-xs text-muted-foreground"
								>Sealed (additionalProperties: false)</span
							>
						</label>
					</div>
				{/if}
			</div>
		</div>

		<!-- ── Array node ─────────────────────────────────────────────────── -->
	{:else if node.kind === 'array'}
		<div class="space-y-2">
			<!-- Items schema -->
			<div class="rounded-md border border-border/60 bg-muted/20 p-2">
				<p class="mb-2 text-xs font-medium text-muted-foreground">Items schema</p>
				<BuilderNodeEditor
					node={node.items}
					onNodeChange={(n) => setArrayItems(n)}
					allowRootScalar={true}
					{definitions}
					{readonly}
					_depth={_depth + 1}
					{onEditRaw}
				/>
			</div>

			<!-- Array constraints / meta -->
			<div class="rounded-md border border-border/40">
				<button
					type="button"
					class="flex w-full items-center justify-between px-3 py-2 text-xs text-muted-foreground hover:text-foreground"
					onclick={() => (metaExpanded = !metaExpanded)}
				>
					<span>Options &amp; constraints</span>
					{#if metaExpanded}
						<ChevronDown class="size-3.5" />
					{:else}
						<ChevronRight class="size-3.5" />
					{/if}
				</button>
				{#if metaExpanded}
					<div class="space-y-2.5 border-t border-border/40 px-3 pb-3 pt-2">
						<div class="space-y-1">
							<Label class="text-xs text-muted-foreground">Description (optional)</Label>
							<Input
								type="text"
								value={node.description ?? ''}
								placeholder="What this array contains…"
								disabled={readonly}
								oninput={(e) =>
									setArrayProp(
										'description',
										(e.currentTarget as HTMLInputElement).value || undefined
									)}
							/>
						</div>
						<div class="grid grid-cols-2 gap-2">
							<div class="space-y-1">
								<Label class="text-xs text-muted-foreground">Min items</Label>
								<Input
									type="number"
									min="0"
									value={node.minItems ?? ''}
									placeholder="—"
									disabled={readonly}
									oninput={(e) =>
										setArrayProp(
											'minItems',
											parseOptionalNumber((e.currentTarget as HTMLInputElement).value)
										)}
								/>
							</div>
							<div class="space-y-1">
								<Label class="text-xs text-muted-foreground">Max items</Label>
								<Input
									type="number"
									min="0"
									value={node.maxItems ?? ''}
									placeholder="—"
									disabled={readonly}
									oninput={(e) =>
										setArrayProp(
											'maxItems',
											parseOptionalNumber((e.currentTarget as HTMLInputElement).value)
										)}
								/>
							</div>
						</div>
						<label class="flex items-center gap-2">
							<Checkbox
								checked={node.nullable}
								disabled={readonly}
								onCheckedChange={(v) => setArrayProp('nullable', v === true)}
							/>
							<span class="text-xs text-muted-foreground">Nullable (type: ["array", "null"])</span>
						</label>
					</div>
				{/if}
			</div>
		</div>

		<!-- ── Scalar node ────────────────────────────────────────────────── -->
	{:else if node.kind === 'scalar'}
		{@const sNode = node as BuilderScalarNode}
		<div class="space-y-2">
			<!-- Type selector -->
			<div class="space-y-1">
				<Label class="text-xs text-muted-foreground">Type</Label>
				<Select.Root
					type="single"
					value={sNode.type}
					disabled={readonly}
					onValueChange={(v) => {
						if (v) setScalarType(v as ScalarJsonType);
					}}
				>
					<Select.Trigger size="sm" class="w-full" disabled={readonly}>
						{SCALAR_TYPES.find((t) => t.value === sNode.type)?.label ?? sNode.type}
					</Select.Trigger>
					<Select.Content>
						{#each SCALAR_TYPES as t (t.value)}
							<Select.Item value={t.value} label={t.label} />
						{/each}
					</Select.Content>
				</Select.Root>
			</div>

			<!-- Widget-kind hint (x-field-kind) -->
			{#if sNode.type === 'string' || sNode.type === 'number' || sNode.type === 'integer' || sNode.type === 'boolean'}
				<div class="space-y-1">
					<Label class="text-xs text-muted-foreground">Widget hint (x-field-kind, optional)</Label>
					<Select.Root
						type="single"
						value={sNode.fieldKindHint ?? ''}
						disabled={readonly}
						onValueChange={(v) =>
							setScalarProp('fieldKindHint', (v || undefined) as FieldKindHint | undefined)}
					>
						<Select.Trigger size="sm" class="w-full" disabled={readonly}>
							{sNode.fieldKindHint
								? (FIELD_KIND_HINTS.find((h) => h.value === sNode.fieldKindHint)?.label ??
									sNode.fieldKindHint)
								: 'None'}
						</Select.Trigger>
						<Select.Content>
							<Select.Item value="" label="None" />
							{#each FIELD_KIND_HINTS as h (h.value)}
								<Select.Item value={h.value} label={h.label} />
							{/each}
						</Select.Content>
					</Select.Root>
				</div>
			{/if}

			<!-- Enum values -->
			<div class="space-y-1">
				<div class="flex items-center justify-between">
					<Label class="text-xs text-muted-foreground">Allowed values (enum)</Label>
					{#if !readonly}
						<button
							type="button"
							class="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
							onclick={addEnumValue}
						>
							<Plus class="size-3" />
							Add
						</button>
					{/if}
				</div>
				{#if sNode.enumValues.length > 0}
					<div class="space-y-1" data-testid="schema-builder-enum-values">
						{#each sNode.enumValues as val, idx (idx)}
							<div class="flex items-center gap-1.5">
								<Input
									type="text"
									value={val}
									placeholder="value"
									class="flex-1 h-7 font-mono text-xs"
									disabled={readonly}
									oninput={(e) =>
										updateEnumValue(idx, (e.currentTarget as HTMLInputElement).value)}
									data-testid="schema-builder-enum-{idx}"
								/>
								{#if !readonly}
									<button
										type="button"
										class="shrink-0 rounded p-0.5 text-muted-foreground hover:text-destructive"
										onclick={() => removeEnumValue(idx)}
										aria-label="Remove value"
									>
										<Trash2 class="size-3.5" />
									</button>
								{/if}
							</div>
						{/each}
					</div>
				{/if}
			</div>

			<!-- Constraints / meta toggle -->
			<div class="rounded-md border border-border/40">
				<button
					type="button"
					class="flex w-full items-center justify-between px-3 py-2 text-xs text-muted-foreground hover:text-foreground"
					onclick={() => (metaExpanded = !metaExpanded)}
				>
					<span>Constraints &amp; meta</span>
					{#if metaExpanded}
						<ChevronDown class="size-3.5" />
					{:else}
						<ChevronRight class="size-3.5" />
					{/if}
				</button>
				{#if metaExpanded}
					<div class="space-y-2.5 border-t border-border/40 px-3 pb-3 pt-2">
						<div class="space-y-1">
							<Label class="text-xs text-muted-foreground">Description (optional)</Label>
							<Input
								type="text"
								value={sNode.description ?? ''}
								placeholder="What this field captures…"
								disabled={readonly}
								oninput={(e) =>
									setScalarProp(
										'description',
										(e.currentTarget as HTMLInputElement).value || undefined
									)}
							/>
						</div>

						{#if sNode.type === 'string'}
							<div class="space-y-1">
								<Label class="text-xs text-muted-foreground">Format (optional)</Label>
								<Input
									type="text"
									value={sNode.format ?? ''}
									placeholder="e.g. date-time, textarea"
									disabled={readonly}
									oninput={(e) =>
										setScalarProp(
											'format',
											(e.currentTarget as HTMLInputElement).value || undefined
										)}
								/>
							</div>
							<div class="grid grid-cols-2 gap-2">
								<div class="space-y-1">
									<Label class="text-xs text-muted-foreground">Min length</Label>
									<Input
										type="number"
										min="0"
										value={sNode.minLength ?? ''}
										placeholder="—"
										disabled={readonly}
										oninput={(e) =>
											setScalarProp(
												'minLength',
												parseOptionalNumber((e.currentTarget as HTMLInputElement).value)
											)}
									/>
								</div>
								<div class="space-y-1">
									<Label class="text-xs text-muted-foreground">Max length</Label>
									<Input
										type="number"
										min="0"
										value={sNode.maxLength ?? ''}
										placeholder="—"
										disabled={readonly}
										oninput={(e) =>
											setScalarProp(
												'maxLength',
												parseOptionalNumber((e.currentTarget as HTMLInputElement).value)
											)}
									/>
								</div>
							</div>
							<div class="space-y-1">
								<Label class="text-xs text-muted-foreground">Pattern (regex, optional)</Label>
								<Input
									type="text"
									value={sNode.pattern ?? ''}
									placeholder="e.g. ^[a-z]+$"
									class="font-mono"
									disabled={readonly}
									oninput={(e) =>
										setScalarProp(
											'pattern',
											(e.currentTarget as HTMLInputElement).value || undefined
										)}
								/>
							</div>
						{/if}

						{#if sNode.type === 'number' || sNode.type === 'integer'}
							<div class="grid grid-cols-2 gap-2">
								<div class="space-y-1">
									<Label class="text-xs text-muted-foreground">Minimum</Label>
									<Input
										type="number"
										value={sNode.minimum ?? ''}
										placeholder="—"
										disabled={readonly}
										oninput={(e) =>
											setScalarProp(
												'minimum',
												parseOptionalNumber((e.currentTarget as HTMLInputElement).value)
											)}
									/>
								</div>
								<div class="space-y-1">
									<Label class="text-xs text-muted-foreground">Maximum</Label>
									<Input
										type="number"
										value={sNode.maximum ?? ''}
										placeholder="—"
										disabled={readonly}
										oninput={(e) =>
											setScalarProp(
												'maximum',
												parseOptionalNumber((e.currentTarget as HTMLInputElement).value)
											)}
									/>
								</div>
							</div>
						{/if}

						<label class="flex items-center gap-2">
							<Checkbox
								checked={sNode.nullable}
								disabled={readonly}
								onCheckedChange={(v) => setScalarProp('nullable', v === true)}
							/>
							<span class="text-xs text-muted-foreground">Nullable (adds "null" to type union)</span>
						</label>
					</div>
				{/if}
			</div>
		</div>

		<!-- ── Union node (oneOf / anyOf) ─────────────────────────────────── -->
	{:else if node.kind === 'union'}
		<div class="space-y-2" data-testid="schema-builder-union">
			<!-- Combinator toggle -->
			<div class="flex items-center gap-2">
				<GitMerge class="size-3.5 shrink-0 text-muted-foreground" />
				<span class="text-xs text-muted-foreground">Combinator</span>
				<div class="flex gap-1">
					<button
						type="button"
						class="rounded border px-2 py-0.5 font-mono text-xs transition-colors {node.combinator ===
						'oneOf'
							? 'border-primary bg-primary/5 text-foreground'
							: 'border-border text-muted-foreground hover:bg-accent/30'}"
						disabled={readonly}
						onclick={() => setUnionProp('combinator', 'oneOf')}
						data-testid="schema-builder-union-oneof"
					>
						oneOf
					</button>
					<button
						type="button"
						class="rounded border px-2 py-0.5 font-mono text-xs transition-colors {node.combinator ===
						'anyOf'
							? 'border-primary bg-primary/5 text-foreground'
							: 'border-border text-muted-foreground hover:bg-accent/30'}"
						disabled={readonly}
						onclick={() => setUnionProp('combinator', 'anyOf')}
						data-testid="schema-builder-union-anyof"
					>
						anyOf
					</button>
				</div>
				{#if node.discriminator}
					<span
						class="ml-auto rounded bg-violet-100 px-1.5 py-0.5 font-mono text-xs text-violet-700 dark:bg-violet-900/30 dark:text-violet-300"
						title="Discriminator property detected"
					>
						discriminator: {node.discriminator}
					</span>
				{/if}
			</div>

			<!-- Variants list -->
			<div class="space-y-1.5" data-testid="schema-builder-union-variants">
				{#each node.variants as variant, idx (idx)}
					{@const isExpanded = expandedVariants.has(idx)}
					{@const tagLabel = node.discriminator
						? variantTagLabel(variant, node.discriminator)
						: null}
					<div
						class="rounded-md border border-border/60 bg-background"
						data-testid="schema-builder-union-variant-{idx}"
					>
						<div class="flex items-center gap-1.5 p-2">
							<button
								type="button"
								class="shrink-0 rounded p-0.5 text-muted-foreground transition-colors hover:text-foreground"
								onclick={() => toggleVariantExpanded(idx)}
								aria-label={isExpanded ? 'Collapse variant' : 'Expand variant'}
								aria-expanded={isExpanded}
							>
								{#if isExpanded}
									<ChevronDown class="size-3.5" />
								{:else}
									<ChevronRight class="size-3.5" />
								{/if}
							</button>
							<span class="flex-1 text-xs text-muted-foreground">
								{#if tagLabel}
									<span class="font-mono text-foreground">{tagLabel}</span>
								{:else}
									Variant {idx + 1}
								{/if}
								<span class="ml-1.5 text-muted-foreground/50">({variant.kind})</span>
							</span>
							{#if node.variants.length > 1 && !readonly}
								<button
									type="button"
									class="shrink-0 rounded p-0.5 text-muted-foreground transition-colors hover:text-destructive"
									onclick={() => removeUnionVariant(idx)}
									aria-label="Remove variant"
									data-testid="schema-builder-union-variant-remove-{idx}"
								>
									<Trash2 class="size-3.5" />
								</button>
							{/if}
						</div>
						{#if isExpanded}
							<div class="border-t border-border/50 p-2 pl-6">
								<BuilderNodeEditor
									node={variant}
									onNodeChange={(n) => updateUnionVariant(idx, n)}
									allowRootScalar={true}
									{definitions}
									{readonly}
									_depth={_depth + 1}
									{onEditRaw}
								/>
							</div>
						{/if}
					</div>
				{/each}
			</div>

			<!-- Add variant button -->
			{#if !readonly}
				<button
					type="button"
					class="flex w-full items-center justify-center gap-1.5 rounded-md border border-dashed border-border py-2 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
					onclick={addUnionVariant}
					data-testid="schema-builder-union-add-variant"
				>
					<Plus class="size-4" />
					Add variant
				</button>
			{/if}

			<!-- Union description -->
			<div class="rounded-md border border-border/40">
				<button
					type="button"
					class="flex w-full items-center justify-between px-3 py-2 text-xs text-muted-foreground hover:text-foreground"
					onclick={() => (metaExpanded = !metaExpanded)}
				>
					<span>Meta</span>
					{#if metaExpanded}
						<ChevronDown class="size-3.5" />
					{:else}
						<ChevronRight class="size-3.5" />
					{/if}
				</button>
				{#if metaExpanded}
					<div class="space-y-2.5 border-t border-border/40 px-3 pb-3 pt-2">
						<div class="space-y-1">
							<Label class="text-xs text-muted-foreground">Description (optional)</Label>
							<Input
								type="text"
								value={node.description ?? ''}
								placeholder="What this union represents…"
								disabled={readonly}
								oninput={(e) =>
									setUnionProp(
										'description',
										(e.currentTarget as HTMLInputElement).value || undefined
									)}
							/>
						</div>
					</div>
				{/if}
			</div>
		</div>

		<!-- ── $ref node ──────────────────────────────────────────────────── -->
	{:else if node.kind === 'ref'}
		<div class="space-y-2" data-testid="schema-builder-ref">
			<!-- Ref name picker -->
			<div class="flex items-center gap-2 rounded-md border border-border/60 bg-muted/20 px-3 py-2">
				<Link class="size-3.5 shrink-0 text-muted-foreground" />
				{#if definitionNames.length > 0}
					<Select.Root
						type="single"
						value={node.name}
						disabled={readonly}
						onValueChange={(v) => {
							if (v) setRefName(v);
						}}
					>
						<Select.Trigger size="sm" class="flex-1" disabled={readonly}>
							{node.name || 'Select definition…'}
						</Select.Trigger>
						<Select.Content>
							{#each definitionNames as defName (defName)}
								<Select.Item value={defName} label={defName} />
							{/each}
						</Select.Content>
					</Select.Root>
				{:else}
					<!-- No definitions available at this call site. -->
					<Input
						type="text"
						value={node.name}
						placeholder="definition name"
						class="flex-1 font-mono text-xs h-7"
						disabled={readonly}
						oninput={(e) => setRefName((e.currentTarget as HTMLInputElement).value)}
						data-testid="schema-builder-ref-name-input"
					/>
					<span class="text-xs text-muted-foreground/60">(no definitions in scope)</span>
				{/if}
			</div>

			<!-- Resolved preview -->
			{#if node.name}
				{#if resolvedRefNode !== null}
					<div class="rounded-md border border-border/40 bg-muted/10 px-3 py-2.5">
						<p class="mb-1.5 text-xs font-medium text-muted-foreground">
							Preview: <code class="font-mono text-foreground">#{node.name}</code>
						</p>
						<SchemaView node={resolvedRefNode} depth={0} />
					</div>
				{:else}
					<p class="flex items-center gap-1.5 text-xs text-muted-foreground/70">
						<AlertCircle class="size-3 shrink-0" />
						Definition <code class="font-mono">{node.name}</code> not found in workflow definitions.
					</p>
				{/if}
			{/if}
		</div>
	{/if}
</div>
