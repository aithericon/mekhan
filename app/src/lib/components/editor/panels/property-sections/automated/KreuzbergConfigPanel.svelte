<script lang="ts">
	import StringListEditor from '../../shared/StringListEditor.svelte';
	import * as Select from '$lib/components/ui/select';
	import { Input } from '$lib/components/ui/input';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import { FormField } from '$lib/components/ui/form-field';

	type Props = {
		config: Record<string, unknown>;
		readonly?: boolean;
		onchange: (config: Record<string, unknown>) => void;
	};

	let { config, readonly = false, onchange }: Props = $props();

	const ocr = $derived((config.ocr as Record<string, unknown>) ?? null);
	const pdf = $derived((config.pdf as Record<string, unknown>) ?? null);
</script>

<div class="space-y-1.5">
	<span class="text-xs font-medium text-muted-foreground">Mode</span>
	<Select.Root
		type="single"
		value={(config.mode as string) ?? 'single'}
		onValueChange={(v) => { if (v) onchange({ ...config, mode: v }); }}
		disabled={readonly}
	>
		<Select.Trigger disabled={readonly}>
			{(config.mode as string) === 'batch' ? 'Batch' : 'Single File'}
		</Select.Trigger>
		<Select.Content>
			<Select.Item value="single" label="Single File" />
			<Select.Item value="batch" label="Batch" />
		</Select.Content>
	</Select.Root>
</div>

{#if (config.mode as string) === 'batch'}
	<div class="space-y-1.5">
		<span class="text-xs font-medium text-muted-foreground">Files (input names)</span>
		<StringListEditor
			items={(config.files as string[]) ?? []}
			{readonly}
			placeholder="Input name"
			onchange={(files) => onchange({ ...config, files })}
		/>
		<p class="text-[9px] italic text-muted-foreground">Empty = use all staged inputs</p>
	</div>
{:else}
	<FormField label="File (input name)" for="kz-file">
		<Input
			id="kz-file"
			type="text"
			value={(config.file as string) ?? ''}
			placeholder="document"
			disabled={readonly}
			oninput={(e) =>
				onchange({ ...config, file: (e.currentTarget as HTMLInputElement).value })}
		/>
	</FormField>
{/if}

<FormField label="MIME Type (optional)" for="kz-mime">
	<Input
		id="kz-mime"
		type="text"
		value={(config.mime_type as string) ?? ''}
		placeholder="Auto-detected"
		disabled={readonly}
		oninput={(e) =>
			onchange({ ...config, mime_type: (e.currentTarget as HTMLInputElement).value || undefined })}
	/>
</FormField>

<label class="flex items-center gap-1.5 text-xs text-muted-foreground">
	<Checkbox
		checked={(config.force_ocr as boolean) ?? false}
		disabled={readonly}
		onCheckedChange={(v) => onchange({ ...config, force_ocr: v })}
	/>
	Force OCR
</label>

<!-- OCR Settings -->
<div class="space-y-1.5 rounded-lg border border-border bg-muted/30 p-2">
	<span class="text-[10px] font-medium text-muted-foreground">OCR Settings</span>
	<Select.Root
		type="single"
		value={(ocr?.backend as string) ?? 'tesseract'}
		onValueChange={(v) => {
			if (v) onchange({ ...config, ocr: { ...(ocr ?? {}), backend: v } });
		}}
		disabled={readonly}
	>
		<Select.Trigger disabled={readonly} class="h-6 px-1.5 py-0.5 text-[10px]">
			{(ocr?.backend as string) === 'paddle-ocr' ? 'PaddleOCR' : 'Tesseract'}
		</Select.Trigger>
		<Select.Content>
			<Select.Item value="tesseract" label="Tesseract" />
			<Select.Item value="paddle-ocr" label="PaddleOCR" />
		</Select.Content>
	</Select.Root>
	<Input
		type="text"
		value={(ocr?.language as string) ?? 'eng'}
		placeholder="Language (ISO 639-3)"
		disabled={readonly}
		oninput={(e) =>
			onchange({
				...config,
				ocr: { ...(ocr ?? {}), language: (e.currentTarget as HTMLInputElement).value }
			})}
		class="h-6 px-1.5 py-0.5 text-[10px]"
	/>
	<label class="flex items-center gap-1 text-[10px] text-muted-foreground">
		<Checkbox
			checked={(ocr?.enable_table_detection as boolean) ?? false}
			disabled={readonly}
			onCheckedChange={(v) =>
				onchange({
					...config,
					ocr: {
						...(ocr ?? {}),
						enable_table_detection: v
					}
				})}
		/>
		Table detection
	</label>
</div>

<!-- PDF Settings -->
<div class="space-y-1.5 rounded-lg border border-border bg-muted/30 p-2">
	<span class="text-[10px] font-medium text-muted-foreground">PDF Settings</span>
	<span class="text-[9px] text-muted-foreground">Passwords (for encrypted PDFs)</span>
	<StringListEditor
		items={(pdf?.passwords as string[]) ?? []}
		{readonly}
		placeholder="Password"
		onchange={(passwords) =>
			onchange({ ...config, pdf: { ...(pdf ?? {}), passwords } })}
	/>
</div>
