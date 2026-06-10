<script lang="ts">
	// Property panel for a StreamSink node: a pull-egress endpoint that consumes
	// its single declared IN channel and re-exposes the bytes at a stable
	// per-instance URL. Reuses the shared ChannelsSection/ChannelEditor with
	// `lockDirection="in"` (a Sink only consumes) and `maxChannels={1}` — v1
	// allows exactly one IN channel (validation-enforced server-side; the cap
	// here keeps the authoring UI from building an unpublishable node).
	import type { StreamSinkNodeData } from '$lib/types/editor';
	import ChannelsSection from './ChannelsSection.svelte';

	type Props = {
		data: StreamSinkNodeData;
		readonly?: boolean;
		onchange: (data: StreamSinkNodeData) => void;
	};

	let { data, readonly = false, onchange }: Props = $props();
</script>

<div class="space-y-2">
	<p class="rounded-md border border-border/50 bg-muted/20 p-2.5 text-sm text-muted-foreground">
		Consumes one upstream stream channel and re-exposes its bytes at a stable per-instance
		egress URL external consumers can tap.
	</p>
</div>

<ChannelsSection
	channels={data.channels ?? []}
	{readonly}
	lockDirection="in"
	maxChannels={1}
	onchange={(channels) =>
		onchange({
			...data,
			// Belt-and-braces: normalize direction + re-apply the v1 single-channel
			// cap so imported/legacy data can never persist an unpublishable shape.
			channels: channels.slice(0, 1).map((c) => ({ ...c, direction: 'in' as const }))
		})}
/>
