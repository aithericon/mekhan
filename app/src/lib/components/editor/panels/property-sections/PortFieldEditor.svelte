<script lang="ts">
	import type { components } from '$lib/api/schema';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import { Input } from '$lib/components/ui/input';
	import { Label } from '$lib/components/ui/label';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import StringListEditor from '../shared/StringListEditor.svelte';
	import * as Select from '$lib/components/ui/select';

	// Editor for a single PortField in a typed Port. Structurally mirrors
	// InputBlockEditor.svelte (human-task field editor) but operates on the
	// typed-ports `PortField` shape (different optionals, FieldKind superset).

	type PortField = components['schemas']['PortField'];
	type FieldKind = components['schemas']['FieldKind'];

	type Props = {
		field: PortField;
		readonly?: boolean;
		onchange: (field: PortField) => void;
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

	const kindLabels: Record<FieldKind, string> = {
		text: 'Text',
		textarea: 'Textarea',
		number: 'Number',
		bool: 'Bool',
		select: 'Select',
		file: 'File',
		signature: 'Signature',
		timestamp: 'Timestamp',
		json: 'JSON'
	};
</script>

<div class="rounded-md border border-border/50 bg-background text-sm">
	<div class="flex items-center gap-2 p-2.5">
		<button
			type="button"
			class="rounded p-0.5 text-muted-foreground transition-colors hover:text-foreground"
			onclick={() => (expanded = !expanded)}
		>
			{#if expanded}
				<ChevronDown class="size-4" />
			{:else}
				<ChevronRight class="size-4" />
			{/if}
		</button>
		<Input
			type="text"
			value={field.label}
			placeholder="Label"
			disabled={readonly}
			oninput={(e) => {
				const label = (e.currentTarget as HTMLInputElement).value;
				const update: PortField = { ...field, label };
				if (!field.name || field.name === slugify(field.label)) {
					update.name = slugify(label);
				}
				onchange(update);
			}}
			class="flex-1"
		/>
		<div class="w-[110px] shrink-0">
			<Select.Root
				type="single"
				value={field.kind}
				onValueChange={(v) => {
					if (v) onchange({ ...field, kind: v as FieldKind });
				}}
				disabled={readonly}
			>
				<Select.Trigger disabled={readonly} class="h-9 px-2 text-sm">
					{kindLabels[field.kind] ?? field.kind}
				</Select.Trigger>
				<Select.Content>
					<Select.Item value="text" label="Text" />
					<Select.Item value="textarea" label="Textarea" />
					<Select.Item value="number" label="Number" />
					<Select.Item value="bool" label="Bool" />
					<Select.Item value="select" label="Select" />
					<Select.Item value="file" label="File" />
					<Select.Item value="signature" label="Signature" />
					<Select.Item value="timestamp" label="Timestamp" />
					<Select.Item value="json" label="JSON" />
				</Select.Content>
			</Select.Root>
		</div>
		<label class="flex items-center gap-1.5">
			<Checkbox
				checked={field.required ?? false}
				disabled={readonly}
				onCheckedChange={(v) => onchange({ ...field, required: v === true })}
			/>
			<span class="text-sm text-muted-foreground">Required</span>
		</label>
		{#if !readonly}
			<button
				type="button"
				class="rounded p-1 text-muted-foreground transition-colors hover:text-destructive"
				onclick={onremove}
			>
				<Trash2 class="size-4" />
			</button>
		{/if}
	</div>

	{#if expanded}
		<div class="space-y-3 border-t border-border/50 p-3">
			<div class="space-y-1.5">
				<Label class="text-sm text-muted-foreground">Field Name (API key)</Label>
				<Input
					type="text"
					value={field.name}
					placeholder="field_name"
					disabled={readonly}
					oninput={(e) =>
						onchange({ ...field, name: (e.currentTarget as HTMLInputElement).value })}
					class="font-mono"
				/>
			</div>

			<div class="space-y-1.5">
				<Label class="text-sm text-muted-foreground">Description</Label>
				<Input
					type="text"
					value={field.description ?? ''}
					placeholder="What this field represents..."
					disabled={readonly}
					oninput={(e) =>
						onchange({
							...field,
							description: (e.currentTarget as HTMLInputElement).value || undefined
						})}
				/>
			</div>

			{#if field.kind === 'select'}
				<div class="space-y-1.5">
					<Label class="text-sm text-muted-foreground">Options</Label>
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
