<script lang="ts">
	/**
	 * SPIKE — config-spec/FieldRenderer.svelte
	 *
	 * Dispatches on spec.kind to the existing shared widgets.  Introduces NO new
	 * widget implementations — it is a pure router onto the same building blocks
	 * used by hand-written section components.
	 *
	 * Props align with the SectionProps contract (scope, resourceScope, readonly)
	 * so FieldRenderer is trivially composable inside SchemaDrivenSection.
	 */

	import type { ConfigFieldSpec } from '$lib/editor/config-spec/types';
	import type { ScopeEntry } from '$lib/editor/guard-scope';

	import { FormField } from '$lib/components/ui/form-field';
	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import * as Select from '$lib/components/ui/select';
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
	// Helpers shared across numeric fields
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

	// ---------------------------------------------------------------------------
	// Derived label — number fields with transform:'clamp01' display a live pct%
	// suffix mirroring the original ProgressUpdateNodeSection behaviour.
	// ---------------------------------------------------------------------------
	const fieldLabel = $derived(
		spec.kind === 'number' && spec.transform === 'clamp01'
			? `${spec.label} — ${Math.round(((value as number) ?? 0) * 100)}%`
			: spec.label
	);

	// Unique-enough id for aria `for` / `id` pairing within the panel.
	const fieldId = $derived(`field-${spec.bind}`);
</script>

{#if spec.kind === 'text'}
	<FormField label={fieldLabel} for={fieldId} description={spec.description}>
		<Input
			id={fieldId}
			type="text"
			class="text-sm"
			value={(value as string) ?? ''}
			placeholder={spec.placeholder}
			disabled={readonly}
			oninput={(e) => onchange((e.currentTarget as HTMLInputElement).value)}
		/>
	</FormField>

{:else if spec.kind === 'textarea'}
	<FormField label={fieldLabel} for={fieldId} description={spec.description}>
		<Textarea
			id={fieldId}
			value={(value as string) ?? ''}
			rows={spec.rows ?? 2}
			placeholder={spec.placeholder}
			disabled={readonly}
			oninput={(e) => {
				const v = (e.currentTarget as HTMLTextAreaElement).value;
				onchange(v === '' ? undefined : v);
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
	<FormField label={fieldLabel} for={fieldId} description={spec.description}>
		<Input
			id={fieldId}
			type="number"
			class="text-sm"
			min={spec.min}
			max={spec.max}
			step={spec.step}
			value={(value as number | undefined) ?? ''}
			disabled={readonly}
			oninput={(e) => {
				const raw = (e.currentTarget as HTMLInputElement).value;
				if (spec.transform === 'clamp01') {
					onchange(clamp01(raw));
				} else if (spec.transform === 'optInt') {
					onchange(optInt(raw));
				} else {
					onchange(raw === '' ? undefined : parseFloat(raw));
				}
			}}
		/>
	</FormField>

{:else if spec.kind === 'bool'}
	<!-- Matches SchemaForm's bool/checkbox pattern: inline label + Checkbox, no FormField wrapper. -->
	<div class="flex flex-col gap-1.5">
		<label class="flex items-center gap-1.5 text-sm text-muted-foreground" for={fieldId}>
			<Checkbox
				id={fieldId}
				checked={(value as boolean) ?? false}
				disabled={readonly}
				onCheckedChange={(v) => onchange(v)}
			/>
			{spec.label}
		</label>
		{#if spec.description}
			<p class="text-sm text-muted-foreground">{spec.description}</p>
		{/if}
	</div>

{:else if spec.kind === 'select'}
	<FormField label={fieldLabel} for={fieldId} description={spec.description}>
		<Select.Root
			type="single"
			value={(value as string) ?? ''}
			onValueChange={(v) => onchange(v ?? '')}
			disabled={readonly}
		>
			<Select.Trigger class="w-full text-sm">
				{spec.options.find((o) => o.value === value)?.label ?? 'Select…'}
			</Select.Trigger>
			<Select.Content>
				{#each spec.options as option (option.value)}
					<Select.Item value={option.value} label={option.label}>
						{option.label}
					</Select.Item>
				{/each}
			</Select.Content>
		</Select.Root>
	</FormField>

{:else if spec.kind === 'ref'}
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
	<!-- ResourcePicker already wraps its own FormField internally -->
	<ResourcePicker
		resourceType={spec.resourceType}
		selected={(value as string) ?? ''}
		label={spec.label}
		typeLabel={spec.typeLabel}
		readonly={readonly}
		onChange={(alias) => onchange(alias)}
	/>

{:else if spec.kind === 'code'}
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
{/if}
