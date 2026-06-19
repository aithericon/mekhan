<script lang="ts">
	// Format-specific metadata renderer for the IMAGE / AUDIO / VIDEO families
	// (png/jpeg/tiff/webp/gif/bmp, mp3/flac/wav/ogg/aac, mp4/mkv/avi/webm). The
	// backend already pre-labels and pre-units `details.fields` (width "1920" px,
	// duration "4:05", sample rate "44100" Hz, bitrate "320" kbps, codec "h265",
	// …). The generic fallback crams those into mono chips; here we lay them out
	// as a clean labeled spec grid. EXIF/ID3 attributes live in the parent
	// dialog's own Attributes section, so we deliberately don't render them.
	import type { MetadataProps } from './types';
	import DetailTable from '../DetailTable.svelte';

	let { mv, onSchemaClick }: MetadataProps = $props();

	const formatLabel = $derived(mv.format ?? null);
	const schema = $derived(mv.schema_fingerprint ?? null);
	const details = $derived(mv.details ?? null);

	// Whether a field value reads as a plain number, so we can right-feel it with
	// tabular-nums (resolutions, sample rates, fps all line up).
	const isNumeric = (v: string) => /^-?\d[\d,]*(\.\d+)?$/.test(v.trim());

	// Width × height collapse into one "1920 × 1080 px" spec when both are present.
	const fields = $derived(details?.fields ?? []);
	const widthField = $derived(fields.find((f) => f.label.toLowerCase() === 'width'));
	const heightField = $derived(fields.find((f) => f.label.toLowerCase() === 'height'));
	const dimsField = $derived(
		widthField && heightField
			? {
					label: 'dimensions',
					value: `${widthField.value} × ${heightField.value}`,
					unit: widthField.unit ?? heightField.unit
				}
			: null
	);
	// The remaining fields, with width/height removed when folded into `dimsField`.
	const specFields = $derived(
		dimsField
			? [dimsField, ...fields.filter((f) => f !== widthField && f !== heightField)]
			: fields
	);
</script>

<!-- Format & schema -->
{#if formatLabel || schema || specFields.length > 0 || (details?.tables?.length ?? 0) > 0}
	<section>
		<h4 class="mb-2 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">Format &amp; schema</h4>
		<div class="flex flex-wrap items-center gap-x-2 gap-y-1.5">
			{#if formatLabel}
				<span class="rounded border border-border bg-background px-1.5 py-0.5 font-mono text-xs">{formatLabel}</span>
			{/if}
			{#if schema?.digest}
				<button
					class="rounded border border-border bg-background px-1.5 py-0.5 font-mono text-xs hover:border-primary hover:text-primary"
					onclick={() => onSchemaClick?.(schema!.digest)}
					title="Filter by this schema fingerprint (v{schema.version})"
				>schema {schema.digest}</button>
			{/if}
		</div>

		{#if specFields.length > 0}
			<dl class="mt-2 grid grid-cols-2 gap-x-4 gap-y-1.5 text-xs sm:grid-cols-3">
				{#each specFields as f}
					<div class="flex flex-col gap-0.5">
						<dt class="text-muted-foreground">{f.label}</dt>
						<dd class="font-medium text-foreground" class:tabular-nums={isNumeric(f.value)}>
							{f.value}{f.unit ? ` ${f.unit}` : ''}
						</dd>
					</div>
				{/each}
			</dl>
		{/if}

		{#if details}
			{#each details.tables ?? [] as t}
				<DetailTable title={t.title} columns={t.columns} rows={t.rows} />
			{/each}
		{/if}
	</section>
{/if}
