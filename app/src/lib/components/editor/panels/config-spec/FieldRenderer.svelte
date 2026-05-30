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

	import type { ConfigFieldSpec, NumberField } from '$lib/editor/config-spec/types';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import type { FieldSpec } from '$lib/fields/spec';

	import { FormField } from '$lib/components/ui/form-field';
	import FieldWidget from '$lib/fields/FieldWidget.svelte';
	import CodeEditor from '../shared/CodeEditor.svelte';
	import RefPicker from '../property-sections/RefPicker.svelte';
	import ResourcePicker from '../property-sections/shared/ResourcePicker.svelte';
	import InsertRefButton from '../property-sections/InsertRefButton.svelte';
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
	};

	let {
		spec,
		value,
		data: _data,
		scope = [],
		resourceScope = [],
		readonly = false,
		onchange
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
	// Derived label — number fields with transform:'clamp01' show a live pct%
	// suffix mirroring the original ProgressUpdateNodeSection behaviour.
	// ---------------------------------------------------------------------------
	const fieldLabel = $derived(
		spec.kind === 'number' && (spec as NumberField).transform === 'clamp01'
			? `${spec.label} — ${Math.round(((value as number) ?? 0) * 100)}%`
			: spec.label
	);

	const fieldId = $derived(`field-${spec.bind}`);

	// ---------------------------------------------------------------------------
	// Build a FieldSpec for the canonical FieldWidget from the ConfigFieldSpec.
	// Only value-input kind branches reach FieldWidget, so authoring-slot fields
	// (ref / resource / code) are never forwarded here.
	// ---------------------------------------------------------------------------
	const fieldWidgetSpec = $derived.by((): FieldSpec => {
		// All ConfigFieldSpec value-input variants share FieldBase; authoring slots
		// are handled before this is used. We build a FieldSpec from the common fields
		// plus per-kind extras.
		const base: FieldSpec = {
			name: spec.bind,
			kind: spec.kind as FieldSpec['kind'],
			label: fieldLabel,
			description: spec.description,
			readonly
		};
		// Per-kind extras
		if (spec.kind === 'text' || spec.kind === 'textarea') {
			return { ...base, placeholder: spec.placeholder, rows: (spec as { rows?: number }).rows };
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
			return { ...base, rows: (spec as { rows?: number }).rows };
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

{:else if spec.kind === 'textarea'}
	<!-- Value-input: textarea needs the InsertRefButton; wrap then delegate. -->
	<FormField label={fieldLabel} for={fieldId} description={spec.description}>
		<FieldWidget
			spec={fieldWidgetSpec}
			{value}
			{readonly}
			onchange={(next) => onchange(next)}
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

{:else}
	<!-- All remaining value-input kinds (text / select / radio / range / rating /
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
