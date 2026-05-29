<script lang="ts">
	import { authFetch } from '$lib/auth/fetch';
	import type { RendererProps } from './types';

	// `value` is an S3 object key (e.g. the agent's `history_ref`:
	// `instances/{id}/agent/turn-1.json`). The raw key is meaningless to a
	// human, so resolve it on demand against the blob-serving endpoint and show
	// the actual JSON — the transcript's assistant `tool_calls` + `role:"tool"`
	// results render verbatim, so you can see the exact shape the agent saw.
	let { value }: RendererProps = $props();

	const key = $derived(String(value));

	let open = $state(false);
	let loading = $state(false);
	let error = $state<string | null>(null);
	let content = $state<string | null>(null);

	async function toggle() {
		open = !open;
		if (!open || content !== null || loading) return;
		loading = true;
		error = null;
		try {
			const r = await authFetch(`/api/v1/files/${key}`);
			if (!r.ok) throw new Error(`fetch failed: ${r.status}`);
			const t = await r.text();
			try {
				content = JSON.stringify(JSON.parse(t), null, 2);
			} catch {
				content = t; // not JSON — show verbatim
			}
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	}
</script>

<div class="flex min-w-0 flex-col gap-1">
	<button
		type="button"
		onclick={toggle}
		class="flex min-w-0 items-center gap-1.5 text-left text-sm text-foreground hover:underline"
		title={key}
	>
		<span class="text-muted-foreground">{open ? '▾' : '▸'}</span>
		<span class="truncate font-mono text-sm text-muted-foreground">{key}</span>
	</button>
	{#if open}
		{#if loading}
			<div class="text-sm text-muted-foreground">Resolving…</div>
		{:else if error}
			<div class="text-sm text-red-500">{error}</div>
		{:else if content !== null}
			<pre
				class="max-h-[60vh] overflow-auto rounded-lg border border-border bg-card p-3 font-mono text-sm text-foreground"><code
					>{content}</code
				></pre>
		{/if}
	{/if}
</div>
