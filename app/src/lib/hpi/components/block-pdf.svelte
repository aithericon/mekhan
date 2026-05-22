<script lang="ts">
	import FileText from '@lucide/svelte/icons/file-text';
	import Download from '@lucide/svelte/icons/download';

	let { url, filename, caption, height = '600px' }: { url: string; filename?: string; caption?: string; height?: string } = $props();

	const displayName = $derived(caption ?? filename ?? 'PDF Document');
</script>

<figure class="rounded-xl border border-border bg-card/70 p-2 shadow-sm">
	<div class="overflow-hidden rounded-lg" style:height>
		<iframe src={url} title={displayName} class="h-full w-full border-0" loading="lazy"></iframe>
	</div>
	<figcaption class="mt-2 flex items-center justify-between gap-2 px-1">
		{#if caption || filename}
			<span class="flex items-center gap-1.5 text-sm text-muted-foreground">
				<FileText class="size-3.5 shrink-0" />
				{displayName}
			</span>
		{:else}
			<span></span>
		{/if}
		<a href={url} download={filename} target="_blank" rel="noopener noreferrer"
		   class="flex items-center gap-1 text-sm text-muted-foreground transition-colors hover:text-primary">
			<Download class="size-3.5" />
			Download
		</a>
	</figcaption>
</figure>
