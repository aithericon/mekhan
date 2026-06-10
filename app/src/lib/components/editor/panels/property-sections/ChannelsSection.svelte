<script lang="ts">
	import type { components } from '$lib/api/schema';
	import Plus from '@lucide/svelte/icons/plus';
	import Radio from '@lucide/svelte/icons/radio';
	import { Button } from '$lib/components/ui/button';
	import ChannelEditor from './ChannelEditor.svelte';
	import { newChannel } from '$lib/editor/channel-authoring';

	// Authoring UI for an AutomatedStep's statically-declared streaming Channels
	// (docs/25). Mirrors PortsSection: a titled list with add/remove, each row a
	// ChannelEditor. The mutated list flows up via `onchange`; the parent persists
	// it to the node data (Yjs-backed), and the node renderer turns each channel
	// into a per-name edge handle.

	type Channel = components['schemas']['Channel'];
	type ChannelDirection = components['schemas']['ChannelDirection'];
	type ChannelTransport = components['schemas']['ChannelTransport'];

	type Props = {
		channels: Channel[];
		readonly?: boolean;
		onchange: (channels: Channel[]) => void;
		/** Force every channel's direction (StreamSource ⇒ 'out', StreamSink ⇒
		 *  'in'). The editor hides the direction picker and new channels are
		 *  seeded with this direction. Absent ⇒ free authoring (AutomatedStep). */
		lockDirection?: ChannelDirection;
		/** Cap on declared channels (StreamSink v1 ⇒ exactly one IN channel).
		 *  The "Add channel" affordance disables once reached. */
		maxChannels?: number;
		/** Restrict the transport picker to this subset (StreamSource ⇒
		 *  jetstream | nats-latest). Absent ⇒ all transports. */
		allowedTransports?: ChannelTransport[];
	};

	let {
		channels,
		readonly = false,
		onchange,
		lockDirection,
		maxChannels,
		allowedTransports
	}: Props = $props();

	const atCap = $derived(maxChannels != null && channels.length >= maxChannels);

	function updateChannel(index: number, channel: Channel) {
		const next = [...channels];
		next[index] = channel;
		onchange(next);
	}

	function removeChannel(index: number) {
		const next = [...channels];
		next.splice(index, 1);
		onchange(next);
	}

	function addChannel() {
		if (atCap) return;
		const seed = newChannel();
		onchange([...channels, lockDirection ? { ...seed, direction: lockDirection } : seed]);
	}
</script>

<div class="space-y-2 border-t border-border/40 pt-3">
	<div class="flex items-center justify-between">
		<span class="flex items-center gap-1.5 text-sm font-medium text-muted-foreground">
			<Radio class="size-3.5" />
			Streaming channels
		</span>
		{#if !readonly}
			<Button
				variant="ghost"
				size="sm"
				onclick={addChannel}
				disabled={atCap}
				title={atCap
					? `This node supports at most ${maxChannels} channel${maxChannels === 1 ? '' : 's'}`
					: undefined}
				class="h-7 gap-1 px-2 text-sm"
			>
				<Plus class="size-3.5" />
				Add channel
			</Button>
		{/if}
	</div>

	{#if channels.length === 0}
		<p class="rounded-md border border-dashed border-border/50 p-3 text-sm text-muted-foreground">
			No streaming channels. Declare one to emit/consume an out-of-band data stream (video, audio,
			large blobs) or dynamic control tokens — each becomes a named edge handle on this node.
		</p>
	{:else}
		<div class="space-y-1.5">
			{#each channels as channel, i (i)}
				<ChannelEditor
					{channel}
					{readonly}
					{lockDirection}
					{allowedTransports}
					onchange={(c) => updateChannel(i, c)}
					onremove={() => removeChannel(i)}
				/>
			{/each}
		</div>
	{/if}
</div>
