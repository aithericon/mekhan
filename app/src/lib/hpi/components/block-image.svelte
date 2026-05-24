<script lang="ts">
	import { authFetch } from '$lib/auth/fetch';

	let { url, alt, caption }: { url: string; alt?: string; caption?: string } = $props();

	// An <img> tag can't carry an Authorization header, so a protected
	// /api/files/... URL 401s under auth (works under dev_noop, breaks in
	// every real deployment). Fetch the bytes via authFetch and hand the tag
	// an object URL instead — same approach as ImageRenderer — revoking it on
	// url change / teardown. A blank URL or a failed load must be surfaced:
	// a silently broken <img> reads as "the workflow forgot the document".
	let src = $state<string | null>(null);
	let failed = $state(false);

	$effect(() => {
		const u = url;
		src = null;
		failed = false;
		if (!u) return;
		const controller = new AbortController();
		let objectUrl: string | null = null;
		authFetch(u, { signal: controller.signal })
			.then((r) => {
				if (!r.ok) throw new Error(`fetch failed: ${r.status}`);
				return r.blob();
			})
			.then((b) => {
				objectUrl = URL.createObjectURL(b);
				src = objectUrl;
			})
			.catch(() => {
				if (controller.signal.aborted) return;
				failed = true;
			});
		return () => {
			controller.abort();
			if (objectUrl) URL.revokeObjectURL(objectUrl);
		};
	});

	const missing = $derived(!url || failed);
</script>

<figure class="rounded-xl border border-border bg-card/70 p-2 shadow-sm">
	{#if missing}
		<div
			data-testid="block-image-missing"
			class="flex flex-col items-center justify-center gap-1 rounded-lg border border-dashed border-destructive/40 bg-destructive/5 px-4 py-8 text-center"
		>
			<span class="text-sm font-medium text-destructive">Image unavailable</span>
			<span class="text-sm text-muted-foreground">
				{url ? 'The file could not be loaded (missing, moved, or access denied).' : 'No file was provided for this block.'}
			</span>
		</div>
	{:else if src}
		<img
			{src}
			alt={alt ?? ''}
			loading="lazy"
			class="mx-auto max-h-[60vh] max-w-full rounded-lg object-contain"
			onerror={() => (failed = true)}
		/>
	{:else}
		<div
			data-testid="block-image-loading"
			class="flex items-center justify-center px-4 py-8 text-sm text-muted-foreground"
		>
			Loading…
		</div>
	{/if}
	{#if caption || alt}
		<figcaption class="mt-2 text-center text-sm text-muted-foreground">
			{caption ?? alt}
		</figcaption>
	{/if}
</figure>
