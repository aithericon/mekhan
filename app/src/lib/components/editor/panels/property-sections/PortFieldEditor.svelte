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

	// Build-exhaustive map: adding or removing a FieldKind from the wire schema
	// causes a TypeScript error here, forcing an update of the kind picker.
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

	// Port-authorable subset: excludes radio / range / rating / date (those are
	// HPI-only kinds that don't map to a port FieldKind wire value). The items
	// below are typed as FieldKind[] so tsc catches any drift with the wire enum.
	// NOTE: wire value 'timestamp' is kept (NOT 'date') — the stored value on
	// PortField must remain the wire FieldKind; adapters convert for display only.
	const PORT_AUTHORABLE_KINDS: FieldKind[] = [
		'text',
		'textarea',
		'number',
		'bool',
		'select',
		'file',
		'signature',
		'timestamp',
		'json'
	];
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
					{#each PORT_AUTHORABLE_KINDS as k (k)}
						<Select.Item value={k} label={kindLabels[k]} />
					{/each}
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
					<!--
						The wire shape is `{value, label}` per option, but this
						editor only exposes `value`; labels default to the
						value when the editor reconstructs them. Rich-label
						authoring goes through hand-edited JSON / a future
						dual-column editor.
					-->
					<StringListEditor
						items={(field.options ?? []).map((o) => o.value)}
						{readonly}
						placeholder="Option value"
						onchange={(values) =>
							onchange({
								...field,
								options: values.map((v) => ({ value: v, label: v }))
							})}
					/>
				</div>
			{/if}
		</div>
	{/if}
</div>
