<script lang="ts">
	let { url, alt, caption }: { url: string; alt?: string; caption?: string } = $props();
</script>

<figure class="rounded-xl border border-border bg-card/70 p-2 shadow-sm">
	<img src={url} alt={alt ?? ''} loading="lazy" class="max-w-full rounded-lg" />
	{#if caption || alt}
		<figcaption class="mt-2 text-center text-sm text-muted-foreground">
			{caption ?? alt}
		</figcaption>
	{/if}
</figure>
