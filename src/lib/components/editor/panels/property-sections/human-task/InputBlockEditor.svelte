<script lang="ts">
	import type { TaskFieldConfig } from '$lib/types/editor';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import StringListEditor from '../../shared/StringListEditor.svelte';
	import { Select, SelectTrigger, SelectContent, SelectItem } from '$lib/components/ui/select';

	type Props = {
		field: TaskFieldConfig;
		readonly?: boolean;
		onchange: (field: TaskFieldConfig) => void;
		onremove: () => void;
	};

	let { field, readonly = false, onchange, onremove }: Props = $props();

	let expanded = $state(false);

	function slugify(label: string): string {
		return label
			.toLowerCase()
			.replace(/[^a-z0-9]+/g, '_')
			.replace(/^_|_$/g, '');
	}

	const kindLabels: Record<string, string> = {
		text: 'Text',
		textarea: 'Textarea',
		number: 'Number',
		select: 'Select',
		checkbox: 'Checkbox',
		file: 'File',
		signature: 'Signature'
	};
</script>

<div class="rounded border border-border/50 bg-background text-[10px]">
	<!-- Collapsed row -->
	<div class="flex items-center gap-1 p-1.5">
		<button
			type="button"
			class="rounded p-0.5 text-muted-foreground transition-colors hover:text-foreground"
			onclick={() => (expanded = !expanded)}
		>
			{#if expanded}
				<ChevronDown class="size-3" />
			{:else}
				<ChevronRight class="size-3" />
			{/if}
		</button>
		<input
			type="text"
			value={field.label}
			placeholder="Label"
			disabled={readonly}
			oninput={(e) => {
				const label = (e.currentTarget as HTMLInputElement).value;
				const update: TaskFieldConfig = { ...field, label };
				// Auto-generate name from label if name is empty or was auto-generated
				if (!field.name || field.name === slugify(field.label)) {
					update.name = slugify(label);
				}
				onchange(update);
			}}
			class="flex-1 rounded border border-input bg-background px-1.5 py-0.5 text-[10px] focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
		/>
		<div class="w-[85px] shrink-0">
			<Select.Root
				type="single"
				value={field.kind}
				onValueChange={(v) => {
					if (v) onchange({ ...field, kind: v as TaskFieldConfig['kind'] });
				}}
				disabled={readonly}
			>
				<SelectTrigger disabled={readonly} class="h-5 px-1 py-0 text-[10px]">
					{kindLabels[field.kind] ?? field.kind}
				</SelectTrigger>
				<SelectContent>
					<SelectItem value="text" label="Text" />
					<SelectItem value="textarea" label="Textarea" />
					<SelectItem value="number" label="Number" />
					<SelectItem value="select" label="Select" />
					<SelectItem value="checkbox" label="Checkbox" />
					<SelectItem value="file" label="File" />
					<SelectItem value="signature" label="Signature" />
				</SelectContent>
			</Select.Root>
		</div>
		<label class="flex items-center gap-0.5">
			<input
				type="checkbox"
				checked={field.required ?? false}
				disabled={readonly}
				onchange={(e) =>
					onchange({
						...field,
						required: (e.currentTarget as HTMLInputElement).checked
					})}
				class="size-3 disabled:cursor-default disabled:opacity-70"
			/>
			<span class="text-muted-foreground">Req</span>
		</label>
		{#if !readonly}
			<button
				type="button"
				class="rounded p-0.5 text-muted-foreground transition-colors hover:text-destructive"
				onclick={onremove}
			>
				<Trash2 class="size-3" />
			</button>
		{/if}
	</div>

	<!-- Expanded section -->
	{#if expanded}
		<div class="space-y-2 border-t border-border/50 p-2">
			<div class="space-y-0.5">
				<span class="text-[9px] text-muted-foreground">Field Name (API key)</span>
				<input
					type="text"
					value={field.name}
					placeholder="field_name"
					disabled={readonly}
					oninput={(e) =>
						onchange({ ...field, name: (e.currentTarget as HTMLInputElement).value })}
					class="w-full rounded border border-input bg-background px-1.5 py-0.5 font-mono text-[10px] focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
				/>
			</div>

			<div class="space-y-0.5">
				<span class="text-[9px] text-muted-foreground">Placeholder</span>
				<input
					type="text"
					value={field.placeholder ?? ''}
					placeholder="Placeholder text..."
					disabled={readonly}
					oninput={(e) =>
						onchange({
							...field,
							placeholder: (e.currentTarget as HTMLInputElement).value || undefined
						})}
					class="w-full rounded border border-input bg-background px-1.5 py-0.5 text-[10px] focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
				/>
			</div>

			{#if field.kind === 'select'}
				<div class="space-y-0.5">
					<span class="text-[9px] text-muted-foreground">Options</span>
					<StringListEditor
						items={field.options ?? []}
						{readonly}
						placeholder="Option value"
						onchange={(options) => onchange({ ...field, options })}
					/>
				</div>
			{/if}
		</div>
	{/if}
</div>
