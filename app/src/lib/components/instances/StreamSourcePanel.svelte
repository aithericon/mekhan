<script lang="ts">
	// Instance-view panel for a `stream_source` node: surfaces the node's STABLE
	// per-instance INGRESS URL(s) — external producers push bytes/items there,
	// which is the whole point of the node — with a copy affordance and a curl
	// hint. The write-side sibling of StreamSinkPanel (the egress view).
	//
	// Data-plane channels take a raw streamed body at `.../data` (the request's
	// Content-Type becomes the stream's content type); control-plane channels
	// take a fused JSON episode `{"items":[...]}` at `.../emit`.
	import { Badge } from '$lib/components/ui/badge';
	import CopyButton from '$lib/components/ui/copy-button/CopyButton.svelte';
	import RadioTower from '@lucide/svelte/icons/radio-tower';
	import type { Channel } from '$lib/api/client';
	import type { ChannelRuntime } from '$lib/stores/instance-marking.svelte';

	type Props = {
		/** Owning workflow instance id (the ingress URL is instance-scoped). */
		instanceId: string;
		/** The stream_source node id (the ingress URL's second key). */
		nodeId: string;
		/** The source's declared OUT channels — one ingress URL each. */
		channels: Channel[];
		/** Per-channel live lifecycle keyed by channel name (best-effort). */
		runtime?: Record<string, ChannelRuntime> | null;
	};

	let { instanceId, nodeId, channels, runtime = null }: Props = $props();

	const origin = $derived(typeof window !== 'undefined' ? window.location.origin : '');

	function ingressUrl(ch: Channel): string {
		const verb = ch.plane === 'control' ? 'emit' : 'data';
		return `${origin}/api/v1/instances/${instanceId}/sources/${nodeId}/channels/${ch.name}/${verb}`;
	}

	function contentType(ch: Channel): string | null {
		const el = ch.element as { type: string; content_type?: string } | undefined;
		return el?.type === 'binary' ? (el.content_type ?? null) : null;
	}

	function curlHint(ch: Channel): string {
		if (ch.plane === 'control') {
			return `curl -X POST -H 'Content-Type: application/json' -d '{"items":[...]}' ${ingressUrl(ch)}`;
		}
		const ct = contentType(ch) ?? 'application/octet-stream';
		return `curl -X POST --data-binary @- -H 'Content-Type: ${ct}' ${ingressUrl(ch)}`;
	}

	function statusLabel(ch: Channel): string | null {
		const rt = runtime?.[ch.name];
		if (!rt || !rt.known) return null;
		const parts: string[] = [rt.opened ? 'opened' : 'idle'];
		if (rt.closed) parts.push('closed');
		return parts.join(' · ');
	}
</script>

<section data-testid="stream-source-panel">
	<h3 class="mb-2 flex items-center gap-2 text-sm font-semibold text-foreground">
		<RadioTower class="size-4 text-muted-foreground" />
		Ingress endpoint{channels.length === 1 ? '' : 's'}
	</h3>

	<div class="space-y-2">
		{#each channels as ch (ch.name)}
			<div class="rounded-md border border-border px-3 py-2 text-sm">
				<div class="mb-1.5 flex items-center gap-2">
					<Badge variant="outline" class="font-mono text-sm font-normal">{ch.name}</Badge>
					<span class="text-muted-foreground">
						{ch.plane === 'control' ? 'POST a JSON episode' : 'POST raw bytes'}
					</span>
					{#if statusLabel(ch)}
						<span class="ml-auto font-mono text-sm font-normal text-muted-foreground"
							>{statusLabel(ch)}</span
						>
					{/if}
				</div>
				<div class="flex items-center gap-1.5">
					<code
						class="min-w-0 flex-1 truncate rounded bg-muted/40 px-2 py-1 font-mono text-foreground"
						title={ingressUrl(ch)}
						data-testid="source-ingress-url">POST {ingressUrl(ch)}</code>
					<CopyButton text={ingressUrl(ch)} title="Copy ingress URL" />
				</div>
				{#if contentType(ch)}
					<div class="mt-1.5 font-mono text-sm text-muted-foreground">{contentType(ch)}</div>
				{/if}
				<div class="mt-1.5 flex items-center gap-1.5">
					<code class="min-w-0 flex-1 truncate font-mono text-sm text-muted-foreground" title={curlHint(ch)}
						>{curlHint(ch)}</code>
					<CopyButton text={curlHint(ch)} title="Copy curl example" />
				</div>
			</div>
		{/each}
	</div>
</section>
