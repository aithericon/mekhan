<script lang="ts">
	/**
	 * config-spec/FieldRenderer.svelte
	 *
	 * Dispatches on spec.kind:
	 *   - Authoring-slot kinds (ref / resource / code) → own widget branches
	 *     (RefPicker, ResourcePicker, CodeEditor). Kept as-is from the spike.
	 *   - Value-input kinds (all canonical FieldKind values) → delegate to
	 *     $lib/fields/FieldWidget, which owns the exhaustive 12-kind renderer.
	 *
	 * Config-spec-specific concerns PRESERVED here:
	 *   - Number `transform: 'clamp01' | 'optInt'` coercion + live-% label
	 *     (ProgressUpdate fraction UX). FieldWidget is presentation-only;
	 *     the transform is applied in this wrapper before calling onchange.
	 *   - Textarea InsertRefButton (scope-ref insertion into the raw text).
	 *   - Code field: RefPicker for Rhai snippets.
	 */

	import type { ConfigFieldSpec, NumberField, SelectField, Port, MappingField, FieldMapping, CustomField } from '$lib/editor/config-spec/types';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import type { FieldSpec } from '$lib/fields/spec';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import type { WorkflowNodeData } from '$lib/types/editor';
	import { resolveCustom } from '$lib/editor/config-spec/custom-registry';

	import { FormField } from '$lib/components/ui/form-field';
	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';
	import { Button } from '$lib/components/ui/button';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import FieldWidget from '$lib/fields/FieldWidget.svelte';
	import CodeEditor from '../shared/CodeEditor.svelte';
	import RefPicker from '../property-sections/RefPicker.svelte';
	import ResourcePicker from '../property-sections/shared/ResourcePicker.svelte';
	import InsertRefButton from '../property-sections/InsertRefButton.svelte';
	import PortsSection from '../property-sections/PortsSection.svelte';
	import { appendSnippet } from '$lib/editor/append-snippet';

	type Props = {
		spec: ConfigFieldSpec;
		/** Current scalar value for this field (typed loosely; each branch narrows). */
		value: unknown;
		/** Full node-data object — passed through for context (not mutated here). */
		data: Record<string, unknown>;
		scope?: ScopeEntry[];
		resourceScope?: ScopeEntry[];
		readonly?: boolean;
		/** Called with the next value for `spec.bind` whenever the field changes. */
		onchange: (next: unknown) => void;
		// ── Section context forwarded to 'custom' slot components ──────────────
		/** Yjs graph binding — forwarded verbatim to custom components. */
		binding?: YjsGraphBinding;
		/** The node id of the node being configured. */
		nodeId?: string;
		/** The template id of the template being configured. */
		templateId?: string;
		/** Callback to select a node in the graph (used by Entrypoints). */
		onselectnode?: (id: string) => void;
	};

	let {
		spec,
		value,
		data,
		scope = [],
		resourceScope = [],
		readonly = false,
		onchange,
		binding,
		nodeId,
		templateId,
		onselectnode
	}: Props = $props();

	// ---------------------------------------------------------------------------
	// Number coercion — config-spec-specific transforms preserved here.
	// FieldWidget emits raw strings (coerceNumbers=false); we apply the transform
	// before forwarding to the host's onchange.
	// ---------------------------------------------------------------------------

	function clamp01(raw: string): number {
		const n = parseFloat(raw);
		if (Number.isNaN(n)) return 0;
		return Math.min(1, Math.max(0, n));
	}

	function optInt(raw: string): number | undefined {
		if (raw === '') return undefined;
		const n = parseInt(raw, 10);
		return Number.isNaN(n) ? undefined : n;
	}

	function handleNumberChange(raw: unknown) {
		if (spec.kind !== 'number') {
			onchange(raw);
			return;
		}
		const numSpec = spec as NumberField;
		const rawStr = String(raw ?? '');
		if (numSpec.transform === 'clamp01') {
			onchange(clamp01(rawStr));
		} else if (numSpec.transform === 'optInt') {
			onchange(optInt(rawStr));
		} else {
			onchange(rawStr === '' ? undefined : parseFloat(rawStr));
		}
	}

	// ---------------------------------------------------------------------------
	// Mapping slot helpers — mirror FailureNodeSection 1:1.
	// Only active when spec.kind === 'mapping'; narrowing is safe because the
	// helpers are only ever called from the mapping branch.
	// ---------------------------------------------------------------------------

	function mappingRows(): FieldMapping[] {
		if (spec.kind !== 'mapping') return [];
		return (data[(spec as MappingField).bind] as FieldMapping[] | undefined) ?? [];
	}

	function setMappingRows(next: FieldMapping[]) {
		if (spec.kind !== 'mapping') return;
		onchange(next);
	}

	function addMappingRow() {
		if (spec.kind !== 'mapping') return;
		const field = spec as MappingField;
		setMappingRows([...mappingRows(), { ...field.newRow }]);
	}

	function updateMappingRow(i: number, patch: Partial<FieldMapping>) {
		setMappingRows(mappingRows().map((r, j) => (j === i ? { ...r, ...patch } : r)));
	}

	function removeMappingRow(i: number) {
		setMappingRows(mappingRows().filter((_, j) => j !== i));
	}

	// ---------------------------------------------------------------------------
	// Derived label — number fields with transform:'clamp01' show a live pct%
	// suffix mirroring the original ProgressUpdateNodeSection behaviour.
	//
	// CustomField has label/bind as optional (it doesn't extend FieldBase); all
	// other kinds always have label and bind. The custom branch never uses these
	// derived values, but we must handle the optional safely for TypeScript.
	// ---------------------------------------------------------------------------
	const specLabel = $derived((spec as { label?: string }).label ?? '');
	const specBind = $derived((spec as { bind?: string }).bind ?? spec.kind);

	const fieldLabel = $derived(
		spec.kind === 'number' && (spec as NumberField).transform === 'clamp01'
			? `${specLabel} — ${Math.round(((value as number) ?? 0) * 100)}%`
			: specLabel
	);

	const fieldId = $derived(`field-${specBind}`);

	// ---------------------------------------------------------------------------
	// Build a FieldSpec for the canonical FieldWidget from the ConfigFieldSpec.
	// Only value-input kind branches reach FieldWidget, so authoring-slot fields
	// (ref / resource / code) are never forwarded here.
	// ---------------------------------------------------------------------------
	const fieldWidgetSpec = $derived.by((): FieldSpec => {
		// All ConfigFieldSpec value-input variants share FieldBase; authoring slots
		// (ref / resource / code / port / mapping / custom) are handled before this
		// is used. We build a FieldSpec from the common fields plus per-kind extras.
		// Custom fields never reach the FieldWidget, but we must produce a valid
		// FieldSpec to satisfy the type (the value is never consumed).
		if (spec.kind === 'custom') {
			return { name: 'custom', kind: 'text', label: '' };
		}
		const base: FieldSpec = {
			name: specBind,
			kind: spec.kind as FieldSpec['kind'],
			label: fieldLabel,
			description: (spec as { description?: string }).description,
			readonly
		};
		// Per-kind extras
		if (spec.kind === 'text') {
			const t = spec as { placeholder?: string; mono?: boolean; valueDefault?: string };
			return { ...base, placeholder: t.placeholder, mono: t.mono };
		}
		if (spec.kind === 'textarea') {
			const ta = spec as { rows?: number; placeholder?: string; testid?: string };
			return { ...base, placeholder: ta.placeholder, rows: ta.rows, testid: ta.testid };
		}
		if (spec.kind === 'number') {
			const n = spec as NumberField;
			return { ...base, min: n.min, max: n.max, step: n.step };
		}
		if (spec.kind === 'select' || spec.kind === 'radio') {
			return { ...base, options: (spec as { options: { value: string; label: string }[] }).options };
		}
		if (spec.kind === 'range') {
			const r = spec as { min?: number; max?: number; step?: number };
			return { ...base, min: r.min, max: r.max, step: r.step };
		}
		if (spec.kind === 'rating') {
			return { ...base, maxRating: (spec as { maxRating?: number }).maxRating };
		}
		if (spec.kind === 'date') {
			return { ...base, includeTime: (spec as { includeTime?: boolean }).includeTime };
		}
		if (spec.kind === 'file') {
			const f = spec as { accept?: string; maxFiles?: number; maxFileSize?: number };
			return { ...base, accept: f.accept, maxFiles: f.maxFiles, maxFileSize: f.maxFileSize };
		}
		if (spec.kind === 'signature') {
			return { ...base, penColor: (spec as { penColor?: string }).penColor };
		}
		if (spec.kind === 'json') {
			const j = spec as { rows?: number; placeholder?: string };
			return { ...base, rows: j.rows, placeholder: j.placeholder };
		}
		// bool / any other canonical kind — no extra props needed
		return base;
	});
</script>

{#if spec.kind === 'ref'}
	<!-- Authoring-slot: RefPicker -->
	<FormField label={fieldLabel} for={fieldId} description={spec.description}>
		<RefPicker
			{scope}
			{resourceScope}
			selected={(value as string | undefined) ?? ''}
			placeholder={spec.placeholder ?? 'Pick reference…'}
			disabled={readonly}
			allowArrayBoundary={spec.allowArrayBoundary ?? false}
			onpick={(entry) => onchange(entry.qualified)}
		/>
		{#if value}
			<p class="mt-1 font-mono text-sm text-muted-foreground">{value as string}</p>
		{/if}
	</FormField>

{:else if spec.kind === 'resource'}
	<!-- Authoring-slot: ResourcePicker (wraps its own FormField) -->
	<ResourcePicker
		resourceType={spec.resourceType}
		selected={(value as string) ?? ''}
		label={spec.label}
		typeLabel={spec.typeLabel}
		readonly={readonly}
		onChange={(alias) => onchange(alias)}
	/>

{:else if spec.kind === 'code'}
	<!-- Authoring-slot: CodeEditor -->
	<FormField label={fieldLabel} for={fieldId} description={spec.description}>
		<CodeEditor
			value={(value as string) ?? ''}
			language={spec.lang}
			{readonly}
			minHeight={spec.minHeight ?? '120px'}
			maxHeight={spec.maxHeight ?? '300px'}
			onchange={(val) => onchange(val)}
		/>
		{#if (spec.lang === 'rhai') && (scope.length > 0 || resourceScope.length > 0)}
			<div class="pt-1">
				<RefPicker
					{scope}
					{resourceScope}
					disabled={readonly}
					placeholder="Insert reference…"
					onpick={(e) =>
						onchange(
							((value as string) ?? '').length > 0
								? `${value} ${e.qualified}`
								: e.qualified
						)}
				/>
			</div>
		{/if}
	</FormField>

{:else if spec.kind === 'port'}
	<!-- Authoring-slot: PortsSection — full port/field editor. -->
	{@const portValue = (value as Port | undefined) ?? spec.default ?? { id: 'out', label: 'Element', fields: [] }}
	<PortsSection
		port={portValue}
		{readonly}
		title={spec.title}
		emptyHint={spec.emptyHint}
		onchange={(next) => onchange(next)}
	/>

{:else if spec.kind === 'mapping'}
	<!-- Authoring-slot: inline FieldMapping[] list editor (mirrors FailureNodeSection). -->
	{@const mappingSpec = spec as MappingField}
	{@const rows = (data[mappingSpec.bind] as FieldMapping[] | undefined) ?? []}
	<div class="space-y-1.5">
		<div class="flex items-center justify-between">
			<span class="text-sm font-medium text-muted-foreground">{mappingSpec.label}</span>
			{#if !readonly}
				<Button
					variant="ghost"
					size="sm"
					onclick={addMappingRow}
					data-testid={mappingSpec.addTestid}
				>
					<Plus class="size-3.5" />
					Add
				</Button>
			{/if}
		</div>
		{#if rows.length === 0}
			<p class="rounded-md border border-dashed border-border/50 p-2 text-sm text-muted-foreground">
				{mappingSpec.emptyHint}
			</p>
		{:else}
			{#each rows as row, i (i)}
				<div class="space-y-1.5 rounded-md border border-border/60 bg-muted/20 p-2">
					<div class="flex items-center gap-2">
						<Input
							type="text"
							value={row.targetField}
							disabled={readonly}
							placeholder={mappingSpec.target.placeholder}
							data-testid={mappingSpec.target.testid}
							oninput={(e) =>
								updateMappingRow(i, {
									targetField: (e.currentTarget as HTMLInputElement).value
								})}
						/>
						{#if !readonly}
							<Button
								variant="ghost"
								size="sm"
								onclick={() => removeMappingRow(i)}
								aria-label="Remove"
							>
								<Trash2 class="size-3.5" />
							</Button>
						{/if}
					</div>
					{#if mappingSpec.source.widget === 'textarea'}
						<Textarea
							value={row.expression}
							disabled={readonly}
							rows={mappingSpec.source.rows ?? 2}
							placeholder={mappingSpec.source.placeholder}
							data-testid={mappingSpec.source.testid}
							oninput={(e) =>
								updateMappingRow(i, {
									expression: (e.currentTarget as HTMLTextAreaElement).value
								})}
						/>
						{#if scope.length > 0}
							<RefPicker
								{scope}
								disabled={readonly}
								placeholder="Insert ref…"
								allowArrayBoundary={mappingSpec.source.allowArrayBoundary ?? false}
								onpick={(e) => {
									updateMappingRow(i, {
										expression: appendSnippet(row.expression, e.qualified)
									});
								}}
							/>
						{/if}
					{:else}
						<!-- refpicker: primary widget, replaces expression; auto-fills targetField when blank -->
						<RefPicker
							{scope}
							disabled={readonly}
							selected={row.expression || undefined}
							placeholder={mappingSpec.source.placeholder}
							allowArrayBoundary={mappingSpec.source.allowArrayBoundary ?? false}
							onpick={(e) =>
								updateMappingRow(i, {
									expression: e.qualified,
									...(mappingSpec.source.autoFillTargetWhenBlank && !row.targetField
										? { targetField: e.field }
										: {})
								})}
						/>
					{/if}
				</div>
			{/each}
		{/if}
		{#if mappingSpec.footer}
			<p class="text-sm text-muted-foreground">{mappingSpec.footer}</p>
		{/if}
	</div>

{:else if spec.kind === 'textarea'}
	<!-- Value-input: textarea — delegates to FieldWidget (which now threads
	     data-testid via fieldWidgetSpec.testid). Config-spec concerns preserved:
	     - clearToUndefined: wrap onchange to collapse '' → undefined.
	     - InsertRefButton: rendered below when scope.length > 0. -->
	{@const clearToUndefined = spec.clearToUndefined ?? false}
	<FormField label={fieldLabel} for={fieldId} description={spec.description}>
		<FieldWidget
			spec={fieldWidgetSpec}
			value={(value as string) ?? ''}
			{readonly}
			onchange={(next) => {
				const v = next as string;
				onchange(clearToUndefined && v === '' ? undefined : v);
			}}
		/>
		{#if scope.length > 0}
			<div class="mt-1.5">
				<InsertRefButton
					{scope}
					{resourceScope}
					disabled={readonly}
					oninsert={(snippet) => onchange(appendSnippet(value as string | undefined, snippet))}
				/>
			</div>
		{/if}
	</FormField>

{:else if spec.kind === 'number'}
	<!-- Value-input: number needs transform coercion; intercept onchange. -->
	<FormField label={fieldLabel} for={fieldId} description={spec.description}>
		<FieldWidget
			spec={fieldWidgetSpec}
			{value}
			{readonly}
			onchange={handleNumberChange}
		/>
	</FormField>

{:else if spec.kind === 'bool'}
	<!-- Value-input: bool uses inline label pattern (no FormField wrapper). -->
	<div class="flex flex-col gap-1.5">
		<label class="flex items-center gap-1.5 text-sm text-muted-foreground" for={fieldId}>
			<FieldWidget
				spec={fieldWidgetSpec}
				{value}
				{readonly}
				onchange={(next) => onchange(next)}
			/>
			{spec.label}
		</label>
		{#if spec.description}
			<p class="text-sm text-muted-foreground">{spec.description}</p>
		{/if}
	</div>

{:else if spec.kind === 'select'}
	<!-- Value-input: select with optional displayDefault (shown when value is undefined). -->
	{@const selectSpec = spec as SelectField}
	{@const displayVal = (value as string | undefined) ?? selectSpec.displayDefault ?? ''}
	<FormField label={fieldLabel} for={fieldId} description={spec.description}>
		<FieldWidget
			spec={fieldWidgetSpec}
			value={displayVal}
			{readonly}
			onchange={(next) => {
				if (next && next !== '') onchange(next);
			}}
		/>
	</FormField>

{:else if spec.kind === 'text'}
	<!-- Value-input: text — delegates to FieldWidget (which now applies font-mono
	     when fieldWidgetSpec.mono is true). Config-spec concerns preserved:
	     - valueDefault read-through: value = (data[bind] ?? spec.valueDefault) passed in.
	     - clearToNull: empty string → null (for processName opt-out).
	     - InsertRefButton: rendered below when scope.length > 0.
	     - mono class: carried via fieldWidgetSpec.mono → FieldWidget applies it. -->
	{@const textSpec = spec as { valueDefault?: string; clearToNull?: boolean }}
	<FormField label={fieldLabel} for={fieldId} description={spec.description}>
		<FieldWidget
			spec={fieldWidgetSpec}
			value={(value as string | undefined) ?? textSpec.valueDefault ?? ''}
			{readonly}
			onchange={(next) => {
				const v = next as string;
				if (textSpec.clearToNull && v === '') {
					onchange(null);
				} else {
					onchange(v);
				}
			}}
		/>
		{#if scope.length > 0}
			<div class="mt-1.5">
				<InsertRefButton
					{scope}
					{resourceScope}
					disabled={readonly}
					oninsert={(snippet) => onchange(appendSnippet(value as string | undefined, snippet))}
				/>
			</div>
		{/if}
	</FormField>

{:else if spec.kind === 'custom'}
	<!-- Escape-hatch: resolve by registry KEY and mount the bespoke component,
	     spreading the full section context so it is indistinguishable from a
	     standalone bespoke section. field.props (static scalar config) is spread
	     LAST; its keys MUST NOT collide with the section-context keys above. -->
	{@const customSpec = spec as CustomField}
	{@const Comp = resolveCustom(customSpec.component)}
	{#if Comp}
		<Comp
			data={data as unknown as WorkflowNodeData}
			{onchange}
			{scope}
			{resourceScope}
			{readonly}
			{binding}
			{nodeId}
			{templateId}
			{onselectnode}
			{...(customSpec.props ?? {})}
		/>
	{:else}
		<!-- Dev guard: unknown registry key. Visible, non-fatal placeholder so a
		     stale/missing key surfaces in the editor instead of silently dropping
		     the whole region. -->
		<p class="text-sm text-destructive">Unknown custom field: {customSpec.component}</p>
	{/if}

{:else}
	<!-- All remaining value-input kinds (radio / range / rating /
	     date / file / signature / json) delegate directly to FieldWidget. -->
	<FormField label={fieldLabel} for={fieldId} description={spec.description}>
		<FieldWidget
			spec={fieldWidgetSpec}
			{value}
			{readonly}
			onchange={(next) => onchange(next)}
		/>
	</FormField>
{/if}
