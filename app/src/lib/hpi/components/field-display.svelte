<script lang="ts">
	// SPDX-License-Identifier: Apache-2.0
	import Star from '@lucide/svelte/icons/star';
	import type { TaskField, SignatureValue } from '../types';
	import { getLinkId, withLinkParam } from './link-context';

	let {
		field,
		fieldValue
	}: {
		field: TaskField;
		fieldValue: unknown;
	} = $props();

	type UploadedFile = { url: string; name: string; size: number; type: string };

	const files = $derived(Array.isArray(fieldValue) ? (fieldValue as UploadedFile[]) : []);

	const sig = $derived(
		(() => {
			if (!fieldValue) return null;
			if (typeof fieldValue === 'object') return fieldValue as SignatureValue;
			try {
				return JSON.parse(String(fieldValue)) as SignatureValue;
			} catch {
				return null;
			}
		})()
	);

	const linkId = getLinkId();

	const ratingVal = $derived(
		field.kind === 'rating' ? (typeof fieldValue === 'number' ? fieldValue : Number(fieldValue)) : 0
	);
	const maxR = $derived(field.max_rating ?? 5);
</script>

<div class="space-y-2 py-1" data-testid={`step-block-input-${field.name}`}>
	<div class="text-sm font-semibold text-foreground/90">{field.label}</div>
	<div class="w-full text-sm text-foreground">
		{#if field.kind === 'checkbox'}
			{fieldValue ? 'Yes' : 'No'}
		{:else if field.kind === 'file'}
			{#if files.length > 0}
				{#each files as f (f.url)}
					<a href={withLinkParam(f.url, linkId)} target="_blank" rel="noopener noreferrer" class="text-primary underline"
						>{f.name}</a
					>
				{/each}
			{:else}
				<span class="text-muted-foreground italic">No files uploaded</span>
			{/if}
		{:else if field.kind === 'signature'}
			{#if sig?.data}
				<div class="rounded-xl border border-border bg-white/80 p-2">
					<img src={sig.data} alt="Signature" class="h-[120px] w-full object-contain" />
				</div>
			{:else}
				<span class="text-muted-foreground italic">No signature provided</span>
			{/if}
		{:else if field.kind === 'rating'}
			<div class="flex items-center gap-0.5">
				{#each Array(maxR) as _, i (i)}
					<Star
						class="size-5 {i < ratingVal
							? 'fill-amber-400 text-amber-400'
							: 'text-muted-foreground/30'}"
					/>
				{/each}
				<span class="ml-1 text-sm text-muted-foreground">{ratingVal}/{maxR}</span>
			</div>
		{:else if fieldValue !== undefined && fieldValue !== null && fieldValue !== ''}
			{fieldValue}
		{:else}
			<span class="text-muted-foreground italic">Not provided</span>
		{/if}
	</div>
</div>
