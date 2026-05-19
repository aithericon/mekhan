<script lang="ts">
	import { catalogueDownloadUrl } from '$lib/api/client';
	import { authFetch } from '$lib/auth/fetch';
	import type { LiveArtifactEntry } from '$lib/api/client';

	interface Props {
		entry: LiveArtifactEntry;
	}
	let { entry }: Props = $props();

	let content = $state<string | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);

	$effect(() => {
		const id = entry.artifact_id ?? entry.id;
		void id;
		content = null;
		loading = true;
		error = null;
		if (!entry.storage_path) {
			loading = false;
			error = 'no storage_path';
			return;
		}
		const url = catalogueDownloadUrl(entry.storage_path);
		const controller = new AbortController();
		authFetch(url, { signal: controller.signal })
			.then((r) => {
				if (!r.ok) throw new Error(`fetch failed: ${r.status}`);
				return r.text();
			})
			.then((t) => {
				try {
					content = JSON.stringify(JSON.parse(t), null, 2);
				} catch {
					content = t;
				}
				loading = false;
			})
			.catch((e) => {
				if (controller.signal.aborted) return;
				error = e instanceof Error ? e.message : String(e);
				loading = false;
			});
		return () => controller.abort();
	});
</script>

<div class="flex flex-col gap-2">
	{#if loading}
		<div class="text-sm text-muted-foreground">Loading JSON…</div>
	{:else if error}
		<div class="text-sm text-red-500">{error}</div>
	{:else if content !== null}
		<pre
			class="max-h-[60vh] overflow-auto rounded-lg border border-border bg-card p-3 font-mono text-sm text-foreground"><code
				>{content}</code
			></pre>
	{/if}
	<p class="truncate text-sm text-muted-foreground">
		{entry.filename}
		{#if entry.size_bytes}
			· {(entry.size_bytes / 1024).toFixed(1)} KB
		{/if}
	</p>
</div>
