<script lang="ts">
	// Property panel for a StreamSource node: a push-ingress endpoint whose
	// declared OUT channels are the node's only ports. Reuses the shared
	// ChannelsSection/ChannelEditor with `lockDirection="out"` (a Source only
	// produces) and the transport picker narrowed to the live byte transports
	// (jetstream | nats-latest) — s3 replay / livekit egress make no sense for
	// an ingress endpoint.
	import type { StreamSourceNodeData } from '$lib/types/editor';
	import ChannelsSection from './ChannelsSection.svelte';

	type Props = {
		data: StreamSourceNodeData;
		readonly?: boolean;
		onchange: (data: StreamSourceNodeData) => void;
	};

	let { data, readonly = false, onchange }: Props = $props();
</script>

<div class="space-y-2">
	<p class="rounded-md border border-border/50 bg-muted/20 p-2.5 text-sm text-muted-foreground">
		External producers push bytes to this node's stable per-instance ingress endpoint; each
		declared channel re-emits them into the workflow as a named OUT handle.
	</p>
</div>

<ChannelsSection
	channels={data.channels ?? []}
	{readonly}
	lockDirection="out"
	allowedTransports={['jetstream', 'nats-latest']}
	onchange={(channels) =>
		onchange({
			...data,
			// Belt-and-braces: the editor locks the picker, but normalize anyway so
			// a legacy/imported channel can never persist with the wrong direction.
			channels: channels.map((c) => ({ ...c, direction: 'out' as const }))
		})}
/>
