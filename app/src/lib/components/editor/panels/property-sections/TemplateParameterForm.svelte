<script lang="ts">
	// Renders a typed form for the parameters declared by a job template version.
	//
	// Loaded lazily when a template ref is picked: fetches the JobTemplateDetail,
	// reads the latest version's `parameters` array, and renders each param using
	// the existing FieldWidget infra. Gracefully degrades for unknown field kinds
	// (renders a plain text input rather than crashing).

	import { untrack } from 'svelte';
	import { FormField } from '$lib/components/ui/form-field';
	import { Input } from '$lib/components/ui/input';
	import FieldWidget from '$lib/fields/FieldWidget.svelte';
	import { getJobTemplate, type JobTemplateDetail, type TemplateParameter } from '$lib/api/job-templates';
	import type { FieldSpec } from '$lib/fields/spec';

	/** A resolved template ref (matches JobTemplatePicker output). */
	interface JobTemplateRef {
		template_id: string;
		version: number | null;
	}

	type Props = {
		/** The currently selected template ref, or null when nothing is selected. */
		templateRef: JobTemplateRef | null;
		/** Current parameter values map (name → value). */
		values: Record<string, unknown>;
		onchange: (next: Record<string, unknown>) => void;
		readonly?: boolean;
	};

	let { templateRef, values, onchange, readonly = false }: Props = $props();

	let detail = $state<JobTemplateDetail | null>(null);
	let loading = $state(false);
	let error = $state<string | null>(null);
	let lastLoadedId: string | null = null;

	async function loadDetail(id: string) {
		loading = true;
		error = null;
		try {
			detail = await getJobTemplate(id);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load template detail';
			detail = null;
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		const id = templateRef?.template_id ?? null;
		if (id !== lastLoadedId) {
			lastLoadedId = id;
			untrack(() => {
				detail = null;
				error = null;
			});
			if (id) loadDetail(id);
		}
	});

	/** The parameters for the version in use (latest if pinned to null). */
	const params = $derived.by((): TemplateParameter[] => {
		if (!detail) return [];
		if (!templateRef?.version) {
			// null → use latest_version
			const v = detail.versions.find((vv) => vv.version === detail!.latest_version);
			return v?.parameters ?? [];
		}
		const v = detail.versions.find((vv) => vv.version === templateRef.version);
		return v?.parameters ?? [];
	});

	/** Map a TemplateParameter.kind string to a FieldWidget-compatible FieldSpec. */
	function toFieldSpec(p: TemplateParameter): FieldSpec {
		const label = p.name + (p.required ? '' : ' (optional)');
		const description = p.description ?? undefined;
		switch (p.kind) {
			case 'bool':
			case 'boolean':
				return { name: p.name, kind: 'bool', label, description, readonly };
			case 'int':
			case 'integer':
			case 'float':
			case 'number':
				return { name: p.name, kind: 'number', label, description, readonly };
			default:
				// string / unknown kinds fall through to text
				return { name: p.name, kind: 'text', label, description, readonly };
		}
	}

	function handleChange(name: string, next: unknown) {
		onchange({ ...values, [name]: next });
	}
</script>

{#if loading}
	<p class="text-sm text-muted-foreground">Loading template parameters…</p>
{:else if error}
	<p class="text-sm text-destructive">{error}</p>
{:else if detail && params.length > 0}
	<div class="space-y-2 pt-2">
		<span class="text-sm font-medium text-muted-foreground">Template parameters</span>
		{#each params as p (p.name)}
			{@const spec = toFieldSpec(p)}
			{@const currentVal = values[p.name] ?? p.default ?? ''}
			{#if spec.kind === 'bool'}
				<div class="flex flex-col gap-1.5">
					<label class="flex items-center gap-1.5 text-sm text-muted-foreground" for={`tp-${p.name}`}>
						<FieldWidget
							{spec}
							value={currentVal}
							{readonly}
							onchange={(v) => handleChange(p.name, v)}
						/>
						{p.name}{p.required ? '' : ' (optional)'}
					</label>
					{#if p.description}
						<p class="text-sm text-muted-foreground">{p.description}</p>
					{/if}
				</div>
			{:else if spec.kind === 'number' || spec.kind === 'text'}
				<FormField label={spec.label ?? p.name} for={`tp-${p.name}`} description={p.description ?? undefined}>
					<FieldWidget
						{spec}
						value={currentVal}
						{readonly}
						onchange={(v) => handleChange(p.name, v)}
					/>
				</FormField>
			{:else}
				<!-- Fallback: unknown kind — plain text input -->
				<FormField label={`${p.name}${p.required ? '' : ' (optional)'}`} for={`tp-${p.name}`} description={p.description ?? undefined}>
					<Input
						id={`tp-${p.name}`}
						type="text"
						class="text-sm"
						value={String(currentVal ?? '')}
						disabled={readonly}
						placeholder={String(p.default ?? '')}
						oninput={(e) => handleChange(p.name, (e.currentTarget as HTMLInputElement).value)}
					/>
				</FormField>
			{/if}
		{/each}
	</div>
{:else if detail && params.length === 0}
	<p class="text-sm italic text-muted-foreground">
		This template has no declared parameters.
	</p>
{/if}
