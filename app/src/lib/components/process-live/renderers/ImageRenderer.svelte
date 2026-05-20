<script lang="ts">
	import { catalogueDownloadUrl } from '$lib/api/client';
	import { authFetch } from '$lib/auth/fetch';
	import type { LiveArtifactEntry } from '$lib/api/client';

	interface Props {
		entry: LiveArtifactEntry;
	}
	let { entry }: Props = $props();

	// An <img> tag can't carry an Authorization header, so the catalogue
	// download URL 401s under Zitadel auth. Fetch the bytes via authFetch and
	// hand the tag an object URL instead; revoke it on change/teardown.
	let src = $state<string | null>(null);
	let error = $state<string | null>(null);

	$effect(() => {
		const id = entry.artifact_id ?? entry.id;
		void id;
		src = null;
		error = null;
		if (!entry.storage_path) {
			error = 'no storage_path';
			return;
		}
		const controller = new AbortController();
		let objectUrl: string | null = null;
		authFetch(catalogueDownloadUrl(entry.storage_path), { signal: controller.signal })
			.then((r) => {
				if (!r.ok) throw new Error(`fetch failed: ${r.status}`);
				return r.blob();
			})
			.then((b) => {
				objectUrl = URL.createObjectURL(b);
				src = objectUrl;
			})
			.catch((e) => {
				if (controller.signal.aborted) return;
				error = e instanceof Error ? e.message : String(e);
			});
		return () => {
			controller.abort();
			if (objectUrl) URL.revokeObjectURL(objectUrl);
		};
	});
</script>

<div class="flex flex-col gap-2">
	{#if error}
		<p class="text-sm text-red-500">{error}</p>
	{:else if src}
		<img
			{src}
			alt={entry.name}
			class="max-h-[60vh] w-full rounded-lg border border-border bg-card object-contain"
			loading="lazy"
		/>
	{:else}
		<div class="text-sm text-muted-foreground">Loading…</div>
	{/if}
	<p class="truncate text-sm text-muted-foreground">
		{entry.filename}
		{#if entry.size_bytes}
			· {(entry.size_bytes / 1024).toFixed(1)} KB
		{/if}
	</p>
</div>
