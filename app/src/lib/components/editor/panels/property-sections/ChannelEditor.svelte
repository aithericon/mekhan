<script lang="ts">
	import type { components } from '$lib/api/schema';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import ArrowRight from '@lucide/svelte/icons/arrow-right';
	import ArrowLeft from '@lucide/svelte/icons/arrow-left';
	import { Input } from '$lib/components/ui/input';
	import { Label } from '$lib/components/ui/label';
	import * as Select from '$lib/components/ui/select';
	import SchemaBuilder from '$lib/schema/SchemaBuilder.svelte';
	import { getWorkflowDefinitions } from '$lib/editor/workflow-definitions.svelte';
	import {
		sanitizeChannelName,
		defaultElement,
		type ElementKind
	} from '$lib/editor/channel-authoring';

	// Editor for a single streaming Channel (docs/25). Structurally mirrors
	// PortFieldEditor.svelte (the typed-port field editor): a collapsible card
	// whose header carries the identity (name + direction) and whose body carries
	// the full shape. The node renderer turns each declared channel into a
	// per-name handle, so `name` here IS the edge handle id.

	type Channel = components['schemas']['Channel'];
	type ChannelDirection = components['schemas']['ChannelDirection'];
	type ChannelPlane = components['schemas']['ChannelPlane'];
	type ChannelTransport = components['schemas']['ChannelTransport'];

	type Props = {
		channel: Channel;
		readonly?: boolean;
		onchange: (channel: Channel) => void;
		onremove: () => void;
		/** Force the channel's direction (StreamSource ⇒ 'out', StreamSink ⇒
		 *  'in'). The direction picker renders as a fixed badge instead of a
		 *  Select. Absent ⇒ free authoring. */
		lockDirection?: ChannelDirection;
		/** Restrict the transport picker to this subset. Absent ⇒ all. */
		allowedTransports?: ChannelTransport[];
	};

	let {
		channel,
		readonly = false,
		onchange,
		onremove,
		lockDirection,
		allowedTransports
	}: Props = $props();

	let expanded = $state(false);

	// Build-exhaustive label maps: dropping/adding an enum variant on the wire
	// breaks compilation here, forcing the picker to be updated in lockstep.
	const directionLabels: Record<ChannelDirection, string> = {
		out: 'Out · produce',
		in: 'In · consume'
	};
	const planeLabels: Record<ChannelPlane, string> = {
		data: 'Data · out-of-band bytes',
		control: 'Control · in-net tokens'
	};
	const elementLabels: Record<ElementKind, string> = {
		json: 'JSON · typed schema',
		binary: 'Binary · tagged bytes',
		any: 'Any · untyped passthrough'
	};
	const transportLabels: Record<ChannelTransport, string> = {
		jetstream: 'JetStream · durable, ordered',
		'nats-latest': 'NATS latest · lossy, latest-only',
		s3: 'S3 / object store · durable, replayable',
		livekit: 'LiveKit · live WebRTC video track (egress-only)'
	};

	const DIRECTIONS: ChannelDirection[] = ['out', 'in'];
	const PLANES: ChannelPlane[] = ['data', 'control'];
	const ELEMENT_KINDS: ElementKind[] = ['binary', 'json', 'any'];
	const ALL_TRANSPORTS: ChannelTransport[] = ['jetstream', 'nats-latest', 's3', 'livekit'];
	// Some node kinds restrict the transports their channels may ride (e.g. a
	// StreamSource ingests live bytes → jetstream | nats-latest only).
	const transports = $derived<ChannelTransport[]>(allowedTransports ?? ALL_TRANSPORTS);

	const transport = $derived<ChannelTransport>(channel.transport ?? 'jetstream');
	// Transport only governs a DATA channel's out-of-band bytes; it's ignored for
	// control channels (which ride the net), so we hide it there.
	const showsTransport = $derived(channel.plane === 'data');

	function patch(next: Partial<Channel>) {
		onchange({ ...channel, ...next });
	}
</script>

<div class="rounded-md border border-border/50 bg-background text-sm">
	<div class="flex items-center gap-2 p-2.5">
		<button
			type="button"
			class="rounded p-0.5 text-muted-foreground transition-colors hover:text-foreground"
			onclick={() => (expanded = !expanded)}
			title={expanded ? 'Collapse' : 'Expand'}
		>
			{#if expanded}
				<ChevronDown class="size-4" />
			{:else}
				<ChevronRight class="size-4" />
			{/if}
		</button>

		<span
			class="shrink-0 text-muted-foreground"
			title={channel.direction === 'out' ? 'Out · produce' : 'In · consume'}
		>
			{#if channel.direction === 'out'}
				<ArrowRight class="size-4" />
			{:else}
				<ArrowLeft class="size-4" />
			{/if}
		</span>

		<Input
			type="text"
			value={channel.name}
			placeholder="channel_name"
			disabled={readonly}
			oninput={(e) =>
				patch({ name: sanitizeChannelName((e.currentTarget as HTMLInputElement).value) })}
			class="flex-1 font-mono"
		/>

		<span
			class="shrink-0 rounded px-1.5 py-0.5 text-xs font-medium {channel.plane === 'data'
				? 'bg-sky-500/10 text-sky-600 dark:text-sky-400'
				: 'bg-violet-500/10 text-violet-600 dark:text-violet-400'}"
			title={planeLabels[channel.plane]}
		>
			{channel.plane}
		</span>

		{#if !readonly}
			<button
				type="button"
				class="rounded p-1 text-muted-foreground transition-colors hover:text-destructive"
				onclick={onremove}
				title="Remove channel"
			>
				<Trash2 class="size-4" />
			</button>
		{/if}
	</div>

	{#if expanded}
		<div class="space-y-3 border-t border-border/40 p-3">
			<div class="grid grid-cols-2 gap-3">
				<div class="space-y-1.5">
					<Label class="text-sm text-muted-foreground">Direction</Label>
					{#if lockDirection}
						<!-- Direction is fixed by the owning node kind (Source produces,
						     Sink consumes) — render the value, not a picker. -->
						<div
							class="flex h-9 items-center rounded-md border border-border/60 bg-muted/30 px-2 text-sm text-muted-foreground"
							title="Direction is fixed by this node type"
						>
							{directionLabels[lockDirection]}
						</div>
					{:else}
						<Select.Root
							type="single"
							value={channel.direction}
							onValueChange={(v) => v && patch({ direction: v as ChannelDirection })}
							disabled={readonly}
						>
							<Select.Trigger disabled={readonly} class="h-9 px-2 text-sm">
								{directionLabels[channel.direction]}
							</Select.Trigger>
							<Select.Content>
								{#each DIRECTIONS as d (d)}
									<Select.Item value={d} label={directionLabels[d]} />
								{/each}
							</Select.Content>
						</Select.Root>
					{/if}
				</div>

				<div class="space-y-1.5">
					<Label class="text-sm text-muted-foreground">Plane</Label>
					<Select.Root
						type="single"
						value={channel.plane}
						onValueChange={(v) => v && patch({ plane: v as ChannelPlane })}
						disabled={readonly}
					>
						<Select.Trigger disabled={readonly} class="h-9 px-2 text-sm">
							{planeLabels[channel.plane]}
						</Select.Trigger>
						<Select.Content>
							{#each PLANES as p (p)}
								<Select.Item value={p} label={planeLabels[p]} />
							{/each}
						</Select.Content>
					</Select.Root>
				</div>
			</div>

			<div class="space-y-1.5">
				<Label class="text-sm text-muted-foreground">Element type</Label>
				<Select.Root
					type="single"
					value={channel.element.type}
					onValueChange={(v) => v && patch({ element: defaultElement(v as ElementKind) })}
					disabled={readonly}
				>
					<Select.Trigger disabled={readonly} class="h-9 px-2 text-sm">
						{elementLabels[channel.element.type]}
					</Select.Trigger>
					<Select.Content>
						{#each ELEMENT_KINDS as k (k)}
							<Select.Item value={k} label={elementLabels[k]} />
						{/each}
					</Select.Content>
				</Select.Root>
			</div>

			{#if channel.element.type === 'binary'}
				<div class="space-y-1.5">
					<Label class="text-sm text-muted-foreground">Content type</Label>
					<Input
						type="text"
						value={channel.element.content_type}
						placeholder="application/octet-stream"
						disabled={readonly}
						oninput={(e) =>
							patch({
								element: {
									type: 'binary',
									content_type: (e.currentTarget as HTMLInputElement).value
								}
							})}
						class="font-mono"
					/>
					<p class="text-xs text-muted-foreground">
						MIME tag for the bytes — drives the runtime player dispatch (e.g.
						<code>audio/L16</code> → Web Audio, <code>audio/mp4;codecs="mp4a.40.2"</code> → MSE).
					</p>
				</div>
			{:else if channel.element.type === 'json'}
				<div class="space-y-1.5">
					<Label class="text-sm text-muted-foreground">Element schema</Label>
					<div class="rounded-md border border-border/50 p-2">
						<SchemaBuilder
							schema={channel.element.schema}
							definitions={getWorkflowDefinitions()}
							onchange={(schema) => patch({ element: { type: 'json', schema } })}
						/>
					</div>
					<p class="text-xs text-muted-foreground">
						The compiler typechecks each emitted element against this schema.
					</p>
				</div>
			{:else}
				<p class="text-xs text-muted-foreground">
					Untyped passthrough — no schema or content-type enforcement.
				</p>
			{/if}

			{#if showsTransport}
				<div class="space-y-1.5">
					<Label class="text-sm text-muted-foreground">Transport</Label>
					<Select.Root
						type="single"
						value={transport}
						onValueChange={(v) => v && patch({ transport: v as ChannelTransport })}
						disabled={readonly}
					>
						<Select.Trigger disabled={readonly} class="h-9 px-2 text-sm">
							{transportLabels[transport]}
						</Select.Trigger>
						<Select.Content>
							{#each transports as t (t)}
								<Select.Item value={t} label={transportLabels[t]} />
							{/each}
						</Select.Content>
					</Select.Root>
					<p class="text-xs text-muted-foreground">
						Out-of-band byte transport. Both executors dispatch the matching adapter off the
						producer's <code>open</code> descriptor — zero SDK change.
					</p>
				</div>
			{/if}
		</div>
	{/if}
</div>
