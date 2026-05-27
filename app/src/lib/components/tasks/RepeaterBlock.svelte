<script lang="ts">
	// Feature B — render N copies of a sub-form, one per element of an
	// upstream array. The parent TaskForm owns `formData` / `errors`;
	// this component receives them as bindable props plus a slim set
	// of callbacks for value/error mutation. Row state lives at
	// `formData[output_slug]: Array<Record<string, unknown>>` and per-row
	// errors use the key `<output_slug>.<row>.<field>`.
	import type { TaskField } from '$lib/hpi/types';
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
		asItemsArray
	} from './task-form-values.svelte.ts';

	interface Props {
		items_ref: string;
		item_label_ref?: string;
		fields: TaskField[];
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
		fields,
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
</script>

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
						{#each fields as field}
							{@const errKey = `${output_slug}.${rowIndex}.${field.name}`}
							{@const fieldId = `field-${output_slug}-${rowIndex}-${field.name}`}
							<div class="space-y-2" data-testid={`repeater-${output_slug}-${rowIndex}-${field.name}`}>
								<Label for={fieldId} class="text-sm font-medium text-foreground">
									{field.label}
									{#if field.required}
										<span class="text-primary">*</span>
									{/if}
								</Label>

								{#if field.kind === 'textarea'}
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
								{:else if field.kind === 'checkbox'}
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
								{:else if field.kind === 'select'}
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
									<!-- text / number / fallback -->
									<Input
										id={fieldId}
										data-testid={`field-${errKey}`}
										type={field.kind === 'number' ? 'number' : 'text'}
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
						{/each}
					</div>
				</li>
			{/each}
		</ul>
	{/if}
</div>
