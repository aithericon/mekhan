<script lang="ts">
	import StringListEditor from '../../shared/StringListEditor.svelte';
	import * as Select from '$lib/components/ui/select';

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
	<div class="space-y-1.5">
		<label for="kz-file" class="text-xs font-medium text-muted-foreground"
			>File (input name)</label
		>
		<input
			id="kz-file"
			type="text"
			value={(config.file as string) ?? ''}
			placeholder="document"
			disabled={readonly}
			oninput={(e) =>
				onchange({ ...config, file: (e.currentTarget as HTMLInputElement).value })}
			class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
		/>
	</div>
{/if}

<div class="space-y-1.5">
	<label for="kz-mime" class="text-xs font-medium text-muted-foreground"
		>MIME Type (optional)</label
	>
	<input
		id="kz-mime"
		type="text"
		value={(config.mime_type as string) ?? ''}
		placeholder="Auto-detected"
		disabled={readonly}
		oninput={(e) =>
			onchange({ ...config, mime_type: (e.currentTarget as HTMLInputElement).value || undefined })}
		class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
	/>
</div>

<label class="flex items-center gap-1.5 text-xs text-muted-foreground">
	<input
		type="checkbox"
		checked={(config.force_ocr as boolean) ?? false}
		disabled={readonly}
		onchange={(e) =>
			onchange({ ...config, force_ocr: (e.currentTarget as HTMLInputElement).checked })}
		class="size-3.5 disabled:cursor-default disabled:opacity-70"
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
	<input
		type="text"
		value={(ocr?.language as string) ?? 'eng'}
		placeholder="Language (ISO 639-3)"
		disabled={readonly}
		oninput={(e) =>
			onchange({
				...config,
				ocr: { ...(ocr ?? {}), language: (e.currentTarget as HTMLInputElement).value }
			})}
		class="w-full rounded border border-input bg-background px-1.5 py-0.5 text-[10px] focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
	/>
	<label class="flex items-center gap-1 text-[10px] text-muted-foreground">
		<input
			type="checkbox"
			checked={(ocr?.enable_table_detection as boolean) ?? false}
			disabled={readonly}
			onchange={(e) =>
				onchange({
					...config,
					ocr: {
						...(ocr ?? {}),
						enable_table_detection: (e.currentTarget as HTMLInputElement).checked
					}
				})}
			class="size-3 disabled:cursor-default disabled:opacity-70"
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
