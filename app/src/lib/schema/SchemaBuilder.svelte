<script lang="ts" module>
	/**
	 * Exported types used by the Integrate stage.
	 *
	 * SchemaBuilder is the primary deliverable of the builder stage. It wraps a
	 * JSON Schema fragment with a full recursive editor: object/array/scalar
	 * nodes with add/remove/rename/reorder, enum values, constraints, meta, and
	 * a raw-JSON escape hatch.
	 *
	 * The component works entirely with JSON Schema as its stored form —
	 * BuilderNode is an internal edit model, not exposed via props.
	 */
	export type { BuilderNode, BuilderField, FieldKindHint } from './builder-model';
</script>

<script lang="ts">
	import { untrack } from 'svelte';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import GripVertical from '@lucide/svelte/icons/grip-vertical';
	import AlertCircle from '@lucide/svelte/icons/alert-circle';
	import Code from '@lucide/svelte/icons/code';
	import { Input } from '$lib/components/ui/input';
	import { Label } from '$lib/components/ui/label';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import * as Select from '$lib/components/ui/select';
	// Self-import for recursion (avoids deprecated <svelte:self>).
	import SchemaBuilder from './SchemaBuilder.svelte';
	import {
		schemaToBuilderNode,
		builderNodeToSchema,
		uniqueFieldName,
		slugifyFieldName,
		type BuilderNode,
		type BuilderObjectNode,
		type BuilderArrayNode,
		type BuilderScalarNode,
		type FieldKindHint,
		type ScalarJsonType
	} from './builder-model';

	// ── Props ────────────────────────────────────────────────────────────────

	type Props = {
		schema: unknown;
		onchange: (schema: unknown) => void;
		/**
		 * When true the root node may be a scalar (Agent use-case).
		 * When false (default) only object/array are valid root shapes; the
		 * root-type toggle is restricted accordingly.
		 */
		allowRootScalar?: boolean;
		/**
		 * Internal: nesting depth, drives indentation and compact sub-node
		 * rendering. Callers leave this absent.
		 */
		_depth?: number;
	};

	let { schema, onchange, allowRootScalar = false, _depth = 0 }: Props = $props();

	// ── Parse schema into builder node ───────────────────────────────────────

	// We use a local editable copy: changes mutate the node via helpers and
	// call onchange(builderNodeToSchema(node)) to propagate upward. We do NOT
	// re-parse on every onchange emission — that would cause an infinite loop.
	// The node is re-parsed only when the _incoming_ schema prop changes from
	// outside (the parent replaced the schema entirely, e.g. raw-JSON edits).

	// Intentional snapshot-only initializer: we track prop changes via $effect
	// below. untrack() suppresses the "state_referenced_locally" warning.
	let node = $state<BuilderNode>(untrack(() => schemaToBuilderNode(schema)));

	// Track the last schema we emitted so we can distinguish parent-driven
	// changes (need re-parse) from our own emissions (should be ignored).
	let lastEmitted = $state<unknown>(null);

	$effect(() => {
		// When the prop changes AND it's not something we just emitted, re-parse.
		// Reading `schema` here makes this effect re-run reactively on prop changes.
		const incoming = schema;
		untrack(() => {
			if (incoming !== lastEmitted) {
				node = schemaToBuilderNode(incoming);
			}
		});
	});

	// ── Raw-JSON escape state ─────────────────────────────────────────────────

	let rawText = $state('');
	let rawError = $state('');
	let rawMode = $state(false);

	function enterRawMode() {
		rawText = JSON.stringify(schema, null, 2);
		rawError = '';
		rawMode = true;
	}

	function exitRawMode() {
		try {
			const parsed = JSON.parse(rawText);
			emit(parsed);
			rawMode = false;
		} catch (e) {
			rawError = `Invalid JSON: ${(e as Error).message}`;
		}
	}

	// ── Emit helper ──────────────────────────────────────────────────────────

	function emit(newSchema: unknown) {
		lastEmitted = newSchema;
		onchange(newSchema);
	}

	function emitNode(n: BuilderNode) {
		node = n;
		emit(builderNodeToSchema(n));
	}

	// ── Root type switcher ───────────────────────────────────────────────────

	type RootKind = 'object' | 'array' | 'scalar';

	const rootKind = $derived<RootKind>(
		node.kind === 'array'
			? 'array'
			: node.kind === 'scalar'
				? 'scalar'
				: 'object'
	);

	function setRootKind(k: RootKind) {
		if (k === rootKind) return;
		if (k === 'object') {
			emitNode({ kind: 'object', fields: [], required: new Set(), sealed: false, nullable: false });
		} else if (k === 'array') {
			emitNode({
				kind: 'array',
				items: { kind: 'object', fields: [], required: new Set(), sealed: false, nullable: false },
				nullable: false
			});
		} else {
			emitNode({ kind: 'scalar', type: 'string', nullable: false, enumValues: [] });
		}
	}

	// ── Object-node helpers ───────────────────────────────────────────────────

	function addField() {
		if (node.kind !== 'object') return;
		const names = node.fields.map((f) => f.name);
		const name = uniqueFieldName(names);
		emitNode({
			...node,
			fields: [
				...node.fields,
				{ name, node: { kind: 'scalar', type: 'string', nullable: false, enumValues: [] } }
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
		emitNode({ ...node, fields, required });
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
		emitNode({ ...node, fields, required });
	}

	function updateFieldNode(idx: number, childNode: BuilderNode) {
		if (node.kind !== 'object') return;
		const fields = node.fields.map((f, i) => (i === idx ? { ...f, node: childNode } : f));
		emitNode({ ...node, fields });
	}

	function toggleRequired(fieldName: string, checked: boolean) {
		if (node.kind !== 'object') return;
		const required = new Set(node.required);
		if (checked) required.add(fieldName);
		else required.delete(fieldName);
		emitNode({ ...node, required });
	}

	function moveField(from: number, to: number) {
		if (node.kind !== 'object') return;
		const fields = [...node.fields];
		const [item] = fields.splice(from, 1);
		fields.splice(to, 0, item);
		emitNode({ ...node, fields });
	}

	function setObjectProp<K extends keyof BuilderObjectNode>(key: K, value: BuilderObjectNode[K]) {
		if (node.kind !== 'object') return;
		emitNode({ ...node, [key]: value });
	}

	// ── Array-node helpers ────────────────────────────────────────────────────

	function setArrayItems(itemsNode: BuilderNode) {
		if (node.kind !== 'array') return;
		emitNode({ ...node, items: itemsNode });
	}

	function setArrayProp<K extends keyof BuilderArrayNode>(key: K, value: BuilderArrayNode[K]) {
		if (node.kind !== 'array') return;
		emitNode({ ...node, [key]: value });
	}

	// ── Scalar-node helpers ───────────────────────────────────────────────────

	function setScalarProp<K extends keyof BuilderScalarNode>(
		key: K,
		value: BuilderScalarNode[K]
	) {
		if (node.kind !== 'scalar') return;
		emitNode({ ...node, [key]: value });
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
		emitNode(next);
	}

	// ── Enum helpers ──────────────────────────────────────────────────────────

	function addEnumValue() {
		if (node.kind !== 'scalar') return;
		emitNode({ ...node, enumValues: [...node.enumValues, ''] });
	}

	function updateEnumValue(idx: number, value: string) {
		if (node.kind !== 'scalar') return;
		const enumValues = node.enumValues.map((v, i) => (i === idx ? value : v));
		emitNode({ ...node, enumValues });
	}

	function removeEnumValue(idx: number) {
		if (node.kind !== 'scalar') return;
		const enumValues = node.enumValues.filter((_, i) => i !== idx);
		emitNode({ ...node, enumValues });
	}

	// ── Expanded-section state ────────────────────────────────────────────────

	/** Which fields are expanded to show their child builder. */
	let expandedFields = $state<Set<string>>(new Set());

	function toggleFieldExpanded(name: string) {
		const next = new Set(expandedFields);
		if (next.has(name)) next.delete(name);
		else next.add(name);
		expandedFields = next;
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

<!-- Root container — no extra margin at depth 0; indented sub-builders clip to their parent field row -->
<div class="min-w-0 space-y-2" data-testid="schema-builder" data-depth={_depth}>

	{#if rawMode}
		<!-- ── Raw JSON editor ─────────────────────────────────────────────── -->
		<div class="space-y-2">
			<div class="flex items-center justify-between">
				<span class="text-xs font-medium text-muted-foreground">Raw JSON Schema</span>
				<button
					type="button"
					class="text-xs text-primary underline-offset-2 hover:underline"
					onclick={exitRawMode}
				>
					Apply & return to builder
				</button>
			</div>
			<textarea
				class="font-mono text-xs w-full min-h-[140px] rounded-md border border-border bg-background px-3 py-2 text-sm shadow-xs outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50 field-sizing-content"
				bind:value={rawText}
				spellcheck="false"
				data-testid="schema-builder-raw-textarea"
			></textarea>
			{#if rawError}
				<p class="flex items-center gap-1.5 text-xs text-destructive" role="alert">
					<AlertCircle class="size-3 shrink-0" />
					{rawError}
				</p>
			{/if}
		</div>
	{:else}
		<!-- ── Root type selector (depth 0 only) ─────────────────────────── -->
		{#if _depth === 0}
			<div class="flex gap-1.5" data-testid="schema-builder-root-kind">
				<button
					type="button"
					class="flex-1 rounded-md border px-2 py-1 text-sm transition-colors {rootKind === 'object' ? 'border-primary bg-primary/5 text-foreground' : 'border-border text-muted-foreground hover:bg-accent/30'}"
					onclick={() => setRootKind('object')}
					data-testid="schema-builder-kind-object"
				>
					Object
				</button>
				<button
					type="button"
					class="flex-1 rounded-md border px-2 py-1 text-sm transition-colors {rootKind === 'array' ? 'border-primary bg-primary/5 text-foreground' : 'border-border text-muted-foreground hover:bg-accent/30'}"
					onclick={() => setRootKind('array')}
					data-testid="schema-builder-kind-array"
				>
					Array
				</button>
				{#if allowRootScalar}
					<button
						type="button"
						class="flex-1 rounded-md border px-2 py-1 text-sm transition-colors {rootKind === 'scalar' ? 'border-primary bg-primary/5 text-foreground' : 'border-border text-muted-foreground hover:bg-accent/30'}"
						onclick={() => setRootKind('scalar')}
						data-testid="schema-builder-kind-scalar"
					>
						Scalar
					</button>
				{/if}
				<button
					type="button"
					class="rounded-md border border-border px-2 py-1 text-muted-foreground transition-colors hover:bg-accent/30"
					title="Edit raw JSON Schema"
					onclick={enterRawMode}
					data-testid="schema-builder-raw-toggle"
				>
					<Code class="size-4" />
				</button>
			</div>
		{/if}

		<!-- ── Raw-only node (unsupported construct) ─────────────────────── -->
		{#if node.kind === 'raw'}
			<div class="flex items-start gap-2 rounded-md border border-amber-200 bg-amber-50 px-3 py-2.5 dark:border-amber-800/40 dark:bg-amber-900/10" data-testid="schema-builder-raw-only">
				<AlertCircle class="mt-0.5 size-4 shrink-0 text-amber-600 dark:text-amber-400" />
				<div class="min-w-0 space-y-1">
					<p class="text-sm text-amber-800 dark:text-amber-300">{node.reason}</p>
					<button
						type="button"
						class="text-xs text-amber-700 underline-offset-2 hover:underline dark:text-amber-400"
						onclick={enterRawMode}
					>
						Edit as raw JSON
					</button>
				</div>
			</div>

		<!-- ── Object node ───────────────────────────────────────────────── -->
		{:else if node.kind === 'object'}
			<div class="space-y-2">
				<!-- Object meta (title / description / nullable / sealed) -->
				{#if _depth === 0}
					<div class="space-y-1.5">
						<Label class="text-xs text-muted-foreground">Title (optional)</Label>
						<Input
							type="text"
							value={node.title ?? ''}
							placeholder="e.g. Result"
							oninput={(e) => setObjectProp('title', (e.currentTarget as HTMLInputElement).value || undefined)}
						/>
					</div>
				{/if}

				<!-- Fields list -->
				<div class="space-y-1.5">
					{#if node.fields.length > 0}
						<div class="space-y-1" role="list" data-testid="schema-builder-fields">
							{#each node.fields as field, idx (field.name + idx)}
								{@const isExpanded = expandedFields.has(field.name + idx)}
								{@const isDraggingOver = dragOverIndex === idx && dragIndex !== idx}
								<div
									role="listitem"
									class="rounded-md border transition-colors {isDraggingOver ? 'border-primary/50 bg-primary/5' : 'border-border/60 bg-background'}"
									draggable="true"
									ondragstart={(e) => onDragStart(idx, e)}
									ondragover={(e) => onDragOver(idx, e)}
									ondrop={() => onDrop(idx)}
									ondragend={onDragEnd}
									data-testid="schema-builder-field-{idx}"
								>
									<!-- Field header row -->
									<div class="flex items-center gap-1.5 p-2">
										<!-- Drag handle -->
										<span class="cursor-grab text-muted-foreground/40 hover:text-muted-foreground active:cursor-grabbing">
											<GripVertical class="size-3.5 shrink-0" />
										</span>

										<!-- Expand toggle for child builder -->
										<button
											type="button"
											class="shrink-0 rounded p-0.5 text-muted-foreground transition-colors hover:text-foreground"
											onclick={() => toggleFieldExpanded(field.name + idx)}
											aria-label={isExpanded ? 'Collapse field' : 'Expand field'}
											aria-expanded={isExpanded}
										>
											{#if isExpanded}
												<ChevronDown class="size-3.5" />
											{:else}
												<ChevronRight class="size-3.5" />
											{/if}
										</button>

										<!-- Field name input -->
										<Input
											type="text"
											value={field.name}
											placeholder="field_name"
											class="flex-1 font-mono text-xs h-7"
											oninput={(e) => renameField(idx, (e.currentTarget as HTMLInputElement).value)}
											data-testid="schema-builder-field-name-{idx}"
										/>

										<!-- Field type badge / selector -->
										<div class="shrink-0">
											{#if field.node.kind === 'raw'}
												<span class="rounded bg-amber-100 px-1.5 py-0.5 text-xs text-amber-700 dark:bg-amber-900/30 dark:text-amber-400">raw</span>
											{:else if field.node.kind === 'object'}
												<span class="rounded bg-slate-100 px-1.5 py-0.5 text-xs text-slate-600 dark:bg-slate-800 dark:text-slate-300">object</span>
											{:else if field.node.kind === 'array'}
												<span class="rounded bg-indigo-100 px-1.5 py-0.5 text-xs text-indigo-700 dark:bg-indigo-900/30 dark:text-indigo-300">array</span>
											{:else if field.node.kind === 'scalar'}
												<span class="rounded bg-blue-100 px-1.5 py-0.5 font-mono text-xs text-blue-700 dark:bg-blue-900/30 dark:text-blue-300">{field.node.type}</span>
											{/if}
										</div>

										<!-- Required toggle -->
										<label class="flex shrink-0 items-center gap-1">
											<Checkbox
												checked={node.required.has(field.name)}
												onCheckedChange={(v) => toggleRequired(field.name, v === true)}
											/>
											<span class="text-xs text-muted-foreground">req</span>
										</label>

										<!-- Remove button -->
										<button
											type="button"
											class="shrink-0 rounded p-0.5 text-muted-foreground transition-colors hover:text-destructive"
											onclick={() => removeField(idx)}
											aria-label="Remove field"
											data-testid="schema-builder-field-remove-{idx}"
										>
											<Trash2 class="size-3.5" />
										</button>
									</div>

									<!-- Expanded child builder -->
									{#if isExpanded}
										<div class="border-t border-border/50 p-2 pl-8">
											<SchemaBuilder
												schema={builderNodeToSchema(field.node)}
												onchange={(s) => updateFieldNode(idx, schemaToBuilderNode(s))}
												allowRootScalar={true}
												_depth={_depth + 1}
											/>
										</div>
									{/if}
								</div>
							{/each}
						</div>
					{/if}

					<!-- Add field button -->
					<button
						type="button"
						class="flex w-full items-center justify-center gap-1.5 rounded-md border border-dashed border-border py-2 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
						onclick={addField}
						data-testid="schema-builder-add-field"
					>
						<Plus class="size-4" />
						Add field
					</button>
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
									oninput={(e) => setObjectProp('description', (e.currentTarget as HTMLInputElement).value || undefined)}
								/>
							</div>
							<label class="flex items-center gap-2">
								<Checkbox
									checked={node.nullable}
									onCheckedChange={(v) => setObjectProp('nullable', v === true)}
								/>
								<span class="text-xs text-muted-foreground">Nullable (type: ["object", "null"])</span>
							</label>
							<label class="flex items-center gap-2">
								<Checkbox
									checked={node.sealed}
									onCheckedChange={(v) => setObjectProp('sealed', v === true)}
								/>
								<span class="text-xs text-muted-foreground">Sealed (additionalProperties: false)</span>
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
					<SchemaBuilder
						schema={builderNodeToSchema(node.items)}
						onchange={(s) => setArrayItems(schemaToBuilderNode(s))}
						allowRootScalar={true}
						_depth={_depth + 1}
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
									oninput={(e) => setArrayProp('description', (e.currentTarget as HTMLInputElement).value || undefined)}
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
										oninput={(e) => setArrayProp('minItems', parseOptionalNumber((e.currentTarget as HTMLInputElement).value))}
									/>
								</div>
								<div class="space-y-1">
									<Label class="text-xs text-muted-foreground">Max items</Label>
									<Input
										type="number"
										min="0"
										value={node.maxItems ?? ''}
										placeholder="—"
										oninput={(e) => setArrayProp('maxItems', parseOptionalNumber((e.currentTarget as HTMLInputElement).value))}
									/>
								</div>
							</div>
							<label class="flex items-center gap-2">
								<Checkbox
									checked={node.nullable}
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
			{@const sNode = node as import('./builder-model').BuilderScalarNode}
			<div class="space-y-2">
				<!-- Type selector -->
				<div class="space-y-1">
					<Label class="text-xs text-muted-foreground">Type</Label>
					<Select.Root
						type="single"
						value={sNode.type}
						onValueChange={(v) => { if (v) setScalarType(v as ScalarJsonType); }}
					>
						<Select.Trigger size="sm" class="w-full">
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
							onValueChange={(v) => setScalarProp('fieldKindHint', (v || undefined) as FieldKindHint | undefined)}
						>
							<Select.Trigger size="sm" class="w-full">
								{sNode.fieldKindHint
									? (FIELD_KIND_HINTS.find((h) => h.value === sNode.fieldKindHint)?.label ?? sNode.fieldKindHint)
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
						<button
							type="button"
							class="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
							onclick={addEnumValue}
						>
							<Plus class="size-3" />
							Add
						</button>
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
										oninput={(e) => updateEnumValue(idx, (e.currentTarget as HTMLInputElement).value)}
										data-testid="schema-builder-enum-{idx}"
									/>
									<button
										type="button"
										class="shrink-0 rounded p-0.5 text-muted-foreground hover:text-destructive"
										onclick={() => removeEnumValue(idx)}
										aria-label="Remove value"
									>
										<Trash2 class="size-3.5" />
									</button>
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
									oninput={(e) => setScalarProp('description', (e.currentTarget as HTMLInputElement).value || undefined)}
								/>
							</div>

							{#if sNode.type === 'string'}
								<div class="space-y-1">
									<Label class="text-xs text-muted-foreground">Format (optional)</Label>
									<Input
										type="text"
										value={sNode.format ?? ''}
										placeholder="e.g. date-time, textarea"
										oninput={(e) => setScalarProp('format', (e.currentTarget as HTMLInputElement).value || undefined)}
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
											oninput={(e) => setScalarProp('minLength', parseOptionalNumber((e.currentTarget as HTMLInputElement).value))}
										/>
									</div>
									<div class="space-y-1">
										<Label class="text-xs text-muted-foreground">Max length</Label>
										<Input
											type="number"
											min="0"
											value={sNode.maxLength ?? ''}
											placeholder="—"
											oninput={(e) => setScalarProp('maxLength', parseOptionalNumber((e.currentTarget as HTMLInputElement).value))}
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
										oninput={(e) => setScalarProp('pattern', (e.currentTarget as HTMLInputElement).value || undefined)}
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
											oninput={(e) => setScalarProp('minimum', parseOptionalNumber((e.currentTarget as HTMLInputElement).value))}
										/>
									</div>
									<div class="space-y-1">
										<Label class="text-xs text-muted-foreground">Maximum</Label>
										<Input
											type="number"
											value={sNode.maximum ?? ''}
											placeholder="—"
											oninput={(e) => setScalarProp('maximum', parseOptionalNumber((e.currentTarget as HTMLInputElement).value))}
										/>
									</div>
								</div>
							{/if}

							<label class="flex items-center gap-2">
								<Checkbox
									checked={sNode.nullable}
									onCheckedChange={(v) => setScalarProp('nullable', v === true)}
								/>
								<span class="text-xs text-muted-foreground">Nullable (adds "null" to type union)</span>
							</label>
						</div>
					{/if}
				</div>
			</div>
		{/if}

		<!-- ── Raw-JSON toggle link (non-depth-0 only, since depth-0 has the icon button) -->
		{#if _depth > 0 && node.kind !== 'raw'}
			<div class="flex justify-end">
				<button
					type="button"
					class="flex items-center gap-1 text-xs text-muted-foreground/60 underline-offset-2 hover:text-muted-foreground hover:underline"
					onclick={enterRawMode}
					data-testid="schema-builder-raw-toggle-nested"
				>
					<Code class="size-3" />
					Edit as raw JSON
				</button>
			</div>
		{/if}
	{/if}
</div>
