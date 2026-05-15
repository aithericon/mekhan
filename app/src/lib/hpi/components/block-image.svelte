<script lang="ts">
	let { url, alt, caption }: { url: string; alt?: string; caption?: string } = $props();

	// A blank URL or a failed load (404 / auth / missing artifact) must be
	// surfaced — a silently broken <img> reads as "the workflow forgot the
	// document" to a reviewer.
	let failed = $state(false);
	const missing = $derived(!url || failed);
</script>

<figure class="rounded-xl border border-border bg-card/70 p-2 shadow-sm">
	{#if missing}
		<div
			data-testid="block-image-missing"
			class="flex flex-col items-center justify-center gap-1 rounded-lg border border-dashed border-destructive/40 bg-destructive/5 px-4 py-8 text-center"
		>
			<span class="text-sm font-medium text-destructive">Image unavailable</span>
			<span class="text-xs text-muted-foreground">
				{url ? 'The file could not be loaded (missing, moved, or access denied).' : 'No file was provided for this block.'}
			</span>
		</div>
	{:else}
		<img
			src={url}
			alt={alt ?? ''}
			loading="lazy"
			class="max-w-full rounded-lg"
			onerror={() => (failed = true)}
		/>
	{/if}
	{#if caption || alt}
		<figcaption class="mt-2 text-center text-sm text-muted-foreground">
			{caption ?? alt}
		</figcaption>
	{/if}
</figure>
