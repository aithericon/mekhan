<script lang="ts">
	// Feature B — render N copies of a sub-task body, one per element of
	// an upstream array. Inner blocks are any TaskBlock variant except
	// another Repeater. Input children render per-row form widgets
	// (state lives at `formData[output_slug]: Array<Record<string,unknown>>`,
	// errors keyed `<output_slug>.<row>.<field>`); display children go
	// through the standard `BlockRenderer` after a per-row placeholder
	// pass that scopes `{{ items_ref-prefix[*].rest }}` to the current
	// row's element.
	import type { TaskBlock, TaskField } from '$lib/hpi/types';
	import { BlockRenderer } from '$lib/hpi';
	import { fromTaskFieldKind } from '$lib/fields/adapters';

	type NonInputBlock = Exclude<TaskBlock, { type: 'input' } | { type: 'repeater' }>;
	import * as Select from '$lib/components/ui/select';
	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import { Label } from '$lib/components/ui/label';
	import { renderMdsvex } from '$lib/mdsvex';
	import { MDSVEX_CLASS } from '$lib/mdsvex-styles';
	import {
		parseRepeaterRef,
		getAtPath,
		asItemsArray,
		interpolateRowPlaceholders
	} from './task-form-values.svelte.ts';

	interface Props {
		items_ref: string;
		item_label_ref?: string;
		blocks: TaskBlock[];
		output_slug: string;
		/** Upstream resolved data — the items array sits at the items_ref pre-`[*]` path. */
		taskData?: Record<string, unknown>;
		/** Read per-row scalar value (text-like). */
		getText: (outputSlug: string, rowIndex: number, fieldName: string) => string;
		/** Read per-row boolean value. */
		getBool: (outputSlug: string, rowIndex: number, fieldName: string) => boolean;
		/** Write per-row value of any kind. */
		setValue: (outputSlug: string, rowIndex: number, fieldName: string, value: unknown) => void;
		/** Parent error map; entries keyed `<output_slug>.<row>.<field>`. */
		errors: Record<string, string>;
	}

	let {
		items_ref,
		item_label_ref,
		blocks,
		output_slug,
		taskData,
		getText,
		getBool,
		setValue,
		errors
	}: Props = $props();

	const parsed = $derived(parseRepeaterRef(items_ref));
	const items = $derived(
		parsed
			? asItemsArray(getAtPath(taskData ?? {}, [parsed.head, ...parsed.pre]))
			: []
	);
	const labelParsed = $derived(item_label_ref ? parseRepeaterRef(item_label_ref) : null);

	function rowLabel(item: unknown, index: number): string {
		if (!labelParsed) return `Item ${index + 1}`;
		const val = getAtPath(item, labelParsed.post);
		if (typeof val === 'string' && val.trim().length > 0) return val;
		if (typeof val === 'number') return String(val);
		return `Item ${index + 1}`;
	}

	/**
	 * Apply per-row placeholder interpolation to a display block's
	 * string-bearing fields. Inputs/Repeaters are passed through
	 * unchanged (Inputs go through the form-value path; nested
	 * Repeaters are rejected at compile time). Other variants get a
	 * shallow clone with their interpolable strings rewritten.
	 */
	function scopedBlock(block: TaskBlock, item: unknown): TaskBlock {
		if (!parsed) return block;
		const sub = (s: string) => interpolateRowPlaceholders(s, parsed, item);
		switch (block.type) {
			case 'mdsvex':
				return { ...block, content: sub(block.content) };
			case 'callout':
				return {
					...block,
					title: block.title !== undefined ? sub(block.title) : block.title,
					content: sub(block.content)
				};
			case 'image':
				return {
					...block,
					url: sub(block.url),
					alt: block.alt !== undefined ? sub(block.alt) : block.alt,
					caption: block.caption !== undefined ? sub(block.caption) : block.caption
				};
			case 'pdf':
				return {
					...block,
					url: sub(block.url),
					filename: block.filename !== undefined ? sub(block.filename) : block.filename,
					caption: block.caption !== undefined ? sub(block.caption) : block.caption
				};
			case 'download':
				return {
					...block,
					downloads: block.downloads.map((d) => ({
						...d,
						url: sub(d.url),
						filename: sub(d.filename),
						description: d.description !== undefined ? sub(d.description) : d.description
					}))
				};
			default:
				return block;
		}
	}
</script>

{#snippet inputField(field: TaskField, rowIndex: number)}
	{@const errKey = `${output_slug}.${rowIndex}.${field.name}`}
	{@const fieldId = `field-${output_slug}-${rowIndex}-${field.name}`}
	{@const canonicalKind = fromTaskFieldKind(field.kind)}
	<div class="space-y-2" data-testid={`repeater-${output_slug}-${rowIndex}-${field.name}`}>
		<Label for={fieldId} class="text-sm font-medium text-foreground">
			{field.label}
			{#if field.required}
				<span class="text-primary">*</span>
			{/if}
		</Label>

		{#if canonicalKind === 'textarea'}
			<Textarea
				id={fieldId}
				data-testid={`field-${errKey}`}
				rows={3}
				placeholder={field.placeholder}
				class="min-h-[80px] rounded-lg bg-white/80"
				value={getText(output_slug, rowIndex, field.name)}
				oninput={(event) =>
					setValue(
						output_slug,
						rowIndex,
						field.name,
						(event.currentTarget as HTMLTextAreaElement).value
					)}
			/>
		{:else if canonicalKind === 'bool'}
			<!-- wire value 'checkbox' → canonical 'bool'; render internals unchanged -->
			<div class="flex items-center gap-3 py-1">
				<Checkbox
					id={fieldId}
					data-testid={`field-${errKey}`}
					checked={getBool(output_slug, rowIndex, field.name)}
					onCheckedChange={(value) =>
						setValue(output_slug, rowIndex, field.name, value === true)}
				/>
				<Label for={fieldId} class="cursor-pointer text-sm text-foreground">
					Yes
				</Label>
			</div>
		{:else if canonicalKind === 'select'}
			<Select.Root
				type="single"
				value={getText(output_slug, rowIndex, field.name)}
				onValueChange={(value) =>
					setValue(output_slug, rowIndex, field.name, value)}
			>
				<Select.Trigger
					id={fieldId}
					data-testid={`field-${errKey}`}
					class="w-full rounded-lg bg-white/80"
				>
					{#if getText(output_slug, rowIndex, field.name)}
						{getText(output_slug, rowIndex, field.name)}
					{:else}
						<span class="text-muted-foreground">Select an option</span>
					{/if}
				</Select.Trigger>
				<Select.Content>
					{#each field.options ?? [] as option (option.value)}
						<Select.Item value={option.value} label={option.label} />
					{/each}
				</Select.Content>
			</Select.Root>
		{:else}
			<!-- text / number / fallback: all other canonical kinds (radio/range/rating/date/
			     file/signature/json) degrade intentionally to a plain text/number Input here.
			     Per-row dotted testids are load-bearing and delegation would break them. -->
			<Input
				id={fieldId}
				data-testid={`field-${errKey}`}
				type={canonicalKind === 'number' ? 'number' : 'text'}
				placeholder={field.placeholder}
				class="rounded-lg bg-white/80"
				value={getText(output_slug, rowIndex, field.name)}
				oninput={(event) =>
					setValue(
						output_slug,
						rowIndex,
						field.name,
						(event.currentTarget as HTMLInputElement).value
					)}
			/>
		{/if}

		{#if field.description_mdsvex}
			<div class={MDSVEX_CLASS}>
				{@html renderMdsvex(field.description_mdsvex)}
			</div>
		{/if}

		{#if errors[errKey]}
			<p class="text-sm text-destructive" data-testid={`field-error-${errKey}`}>
				{errors[errKey]}
			</p>
		{/if}
	</div>
{/snippet}

<div class="space-y-3 py-1" data-testid={`step-block-repeater-${output_slug}`}>
	{#if !parsed}
		<div
			class="rounded-lg border border-amber-200 bg-amber-50 px-3 py-2 text-sm text-amber-700"
			data-testid={`repeater-${output_slug}-malformed`}
		>
			Malformed Repeater reference: <code>{items_ref}</code>
		</div>
	{:else if items.length === 0}
		<div
			class="rounded-lg border border-muted bg-muted/30 px-3 py-2 text-sm text-muted-foreground"
			data-testid={`repeater-${output_slug}-empty`}
		>
			No items to review.
		</div>
	{:else}
		<ul class="flex flex-col gap-3" data-testid={`repeater-${output_slug}-rows`}>
			{#each items as item, rowIndex (rowIndex)}
				<li
					class="rounded-xl border border-border bg-card/40 p-3"
					data-testid={`repeater-${output_slug}-row-${rowIndex}`}
				>
					<div class="mb-2 flex items-center justify-between gap-2">
						<span class="text-base font-semibold text-foreground">
							{rowLabel(item, rowIndex)}
						</span>
						<span class="text-sm text-muted-foreground">
							{rowIndex + 1} / {items.length}
						</span>
					</div>
					<div class="flex flex-col gap-3">
						{#each blocks as block, blockIdx (blockIdx)}
							{#if block.type === 'input'}
								{@render inputField(block.field, rowIndex)}
							{:else if block.type !== 'repeater'}
								<BlockRenderer block={scopedBlock(block, item) as NonInputBlock} />
							{/if}
						{/each}
					</div>
				</li>
			{/each}
		</ul>
	{/if}
</div>
