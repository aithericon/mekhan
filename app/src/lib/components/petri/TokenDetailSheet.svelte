<script lang="ts">
	import { X } from '@lucide/svelte';
	import MonacoEditor from './MonacoEditor.svelte';
	import { CopyButton } from '$lib/components/ui/copy-button';
	import type { Token, PersistedEvent } from '$lib/types/petri';

	interface Props {
		token: Token | null;
		placeName: string;
		open: boolean;
		onClose: () => void;
		/** Full event log — used to resolve created_by_event provenance */
		events?: PersistedEvent[];
		/** Resolve place ID → human name */
		getPlaceName?: (id: string) => string;
		/** Resolve transition ID → human name */
		getTransitionName?: (id: string) => string;
	}

	let { token, placeName, open, onClose, events = [], getPlaceName, getTransitionName }: Props = $props();

	const colorType = $derived(token?.color.type ?? 'Unit');
	const colorValue = $derived((token?.color as any)?.value);

	// Resolve the event that created this token and extract provenance context
	const provenance = $derived.by(() => {
		if (!token?.created_by_event || !events.length) return null;
		const ev = events.find(e => e.sequence === token.created_by_event);
		if (!ev) return null;

		const d = ev.event as any;
		const result: {
			eventType: string;
			sequence: number;
			timestamp: string;
			signalKey?: string;
			workflowId?: string;
			transitionName?: string;
			sourcePlaceName?: string;
			sourceNetId?: string;
			targetNetId?: string;
			targetPlaceName?: string;
			effectHandlerId?: string;
		} = {
			eventType: d.type,
			sequence: ev.sequence,
			timestamp: ev.timestamp,
		};

		if (d.type === 'TokenCreated') {
			result.sourcePlaceName = getPlaceName ? getPlaceName(d.place_id) : d.place_name ?? d.place_id;
			if (d.signal_key) result.signalKey = d.signal_key;
			if (d.workflow_id) result.workflowId = d.workflow_id;
		} else if (d.type === 'TransitionFired' || d.type === 'EffectCompleted') {
			result.transitionName = getTransitionName ? getTransitionName(d.transition_id) : d.transition_name ?? d.transition_id;
			if (d.effect_handler_id) result.effectHandlerId = d.effect_handler_id;
			// Find which place this token was produced into
			const produced = d.produced_tokens as [string, Token][] | undefined;
			if (produced) {
				const entry = produced.find(([, t]) => t.id === token.id);
				if (entry) {
					result.sourcePlaceName = getPlaceName ? getPlaceName(entry[0]) : entry[0];
				}
			}
		} else if (d.type === 'EffectFailed') {
			result.transitionName = getTransitionName ? getTransitionName(d.transition_id) : d.transition_name ?? d.transition_id;
			if (d.effect_handler_id) result.effectHandlerId = d.effect_handler_id;
		} else if (d.type === 'TokenBridgedOut') {
			// This token was produced by a bridge-in on another net — rare to see here
			// but for completeness
			result.sourceNetId = d.target_net_id;
			result.targetPlaceName = d.target_place_name;
		}

		return result;
	});

	const typeBadgeClass = $derived.by(() => {
		switch (colorType) {
			case 'Unit':
				return 'bg-gray-500/15 text-gray-700 dark:text-gray-400';
			case 'Integer':
				return 'bg-violet-500/15 text-violet-700 dark:text-violet-400';
			case 'Data':
				return 'bg-pink-500/15 text-pink-700 dark:text-pink-400';
			default:
				return 'bg-muted text-foreground';
		}
	});

	const formattedData = $derived.by(() => {
		if (!token) return '';
		if (colorType === 'Unit') return '';
		if (colorType === 'Integer') return String(colorValue);
		if (colorType === 'Data') return JSON.stringify(colorValue, null, 2);
		return '';
	});

	const copyableData = $derived.by(() => {
		if (!token) return '';
		if (colorType === 'Unit') return 'Unit';
		if (colorType === 'Integer') return String(colorValue);
		if (colorType === 'Data') return JSON.stringify(colorValue, null, 2);
		return '';
	});

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Escape') {
			onClose();
		}
	}
</script>

<svelte:window onkeydown={handleKeydown} />

{#if open && token}
	<!-- Backdrop -->
	<div
		class="fixed inset-0 bg-black/20 z-40"
		style="right: 608px;"
		onclick={onClose}
		onkeydown={(e) => e.key === 'Escape' && onClose()}
		role="button"
		tabindex="-1"
		aria-label="Close sheet"
	></div>

	<!-- Sheet panel — fills canvas area below toolbar -->
	<div
		class="fixed left-0 bottom-0 z-50 bg-card border-r border-border shadow-2xl flex flex-col"
		style="right: 608px; top: 49px;"
	>
		<!-- Header -->
		<div class="flex items-center justify-between px-4 py-3 border-b border-border bg-muted">
			<div class="flex items-center gap-3">
				<h2 class="text-lg font-semibold text-foreground">Token</h2>
				<span class="px-2 py-0.5 text-xs font-medium rounded {typeBadgeClass}">
					{colorType}
				</span>
				<span class="text-sm text-muted-foreground">
					in <span class="font-medium text-foreground">{placeName}</span>
				</span>
			</div>
			<button
				onclick={onClose}
				class="p-1 rounded hover:bg-muted transition-colors"
				aria-label="Close"
			>
				<X class="w-5 h-5 text-muted-foreground" />
			</button>
		</div>

		<!-- Content -->
		<div class="flex-1 flex flex-col p-4 gap-4 min-h-0">
			<!-- Metadata -->
			<div class="shrink-0 grid grid-cols-2 gap-4">
				<div>
					<h3 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Token ID</h3>
					<div class="flex items-center gap-1">
						<span class="text-xs font-mono text-foreground/80 break-all">{token.id}</span>
						<CopyButton text={token.id} />
					</div>
				</div>
				<div>
					<h3 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Created</h3>
					<span class="text-sm text-foreground">
						{new Date(token.created_at).toLocaleString()}
					</span>
				</div>
				{#if token.created_by_event != null}
					<div>
						<h3 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Created by Event</h3>
						<span class="text-xs font-mono text-foreground/80">#{token.created_by_event}</span>
					</div>
				{/if}
			</div>

			{#if provenance}
				<div class="shrink-0 border border-blue-500/20 rounded-lg p-3 bg-blue-500/5">
					<h3 class="text-xs font-semibold uppercase tracking-wider text-blue-500 mb-2">Provenance</h3>
					<div class="grid grid-cols-2 gap-3">
						<div>
							<span class="text-[10px] uppercase tracking-wider text-muted-foreground">Origin</span>
							<div class="text-xs text-foreground/80">
								<span class="px-1.5 py-0.5 rounded text-[10px] font-medium
									{provenance.eventType === 'TokenCreated' ? 'bg-green-500/15 text-green-500'
									: provenance.eventType === 'EffectCompleted' ? 'bg-emerald-500/15 text-emerald-500'
									: provenance.eventType === 'TransitionFired' ? 'bg-amber-500/15 text-amber-500'
									: provenance.eventType === 'EffectFailed' ? 'bg-red-500/15 text-red-500'
									: 'bg-muted text-foreground'}">
									{provenance.eventType}
								</span>
								<span class="ml-1 font-mono text-muted-foreground">#{provenance.sequence}</span>
							</div>
						</div>
						<div>
							<span class="text-[10px] uppercase tracking-wider text-muted-foreground">When</span>
							<div class="text-xs text-foreground/80">{new Date(provenance.timestamp).toLocaleString()}</div>
						</div>
						{#if provenance.transitionName}
							<div>
								<span class="text-[10px] uppercase tracking-wider text-muted-foreground">Transition</span>
								<div class="text-xs font-medium text-foreground/80">{provenance.transitionName}</div>
							</div>
						{/if}
						{#if provenance.effectHandlerId}
							<div>
								<span class="text-[10px] uppercase tracking-wider text-muted-foreground">Effect Handler</span>
								<div class="text-xs font-mono text-foreground/80">{provenance.effectHandlerId}</div>
							</div>
						{/if}
						{#if provenance.sourcePlaceName}
							<div>
								<span class="text-[10px] uppercase tracking-wider text-muted-foreground">Place</span>
								<div class="text-xs font-medium text-foreground/80">{provenance.sourcePlaceName}</div>
							</div>
						{/if}
						{#if provenance.signalKey}
							<div>
								<span class="text-[10px] uppercase tracking-wider text-muted-foreground">Signal Key</span>
								<div class="flex items-center gap-1">
									<span class="text-xs font-mono text-foreground/80 break-all">{provenance.signalKey}</span>
									<CopyButton text={provenance.signalKey} />
								</div>
							</div>
						{/if}
						{#if provenance.workflowId}
							<div>
								<span class="text-[10px] uppercase tracking-wider text-muted-foreground">Workflow ID</span>
								<div class="flex items-center gap-1">
									<span class="text-xs font-mono text-foreground/80 break-all">{provenance.workflowId}</span>
									<CopyButton text={provenance.workflowId} />
								</div>
							</div>
						{/if}
					</div>
				</div>
			{/if}

			{#if token.reply_routing}
				<div class="shrink-0 border border-rose-500/20 rounded-lg p-3 bg-rose-500/5">
					<h3 class="text-xs font-semibold uppercase tracking-wider text-rose-500 mb-2">Reply Routing</h3>
					<div class="grid grid-cols-2 gap-3">
						{#if token.reply_routing.reply_to}
							<div class="col-span-2">
								<span class="text-[10px] uppercase tracking-wider text-muted-foreground">Reply To (default channel)</span>
								<div class="flex items-center gap-1">
									<span class="text-xs font-mono text-foreground/80">
										{token.reply_routing.reply_to.net_id} / {token.reply_routing.reply_to.place_name}
									</span>
									<CopyButton text="{token.reply_routing.reply_to.net_id}/{token.reply_routing.reply_to.place_name}" />
								</div>
							</div>
						{/if}
						{#if token.reply_routing.reply_channels}
							{#each Object.entries(token.reply_routing.reply_channels) as [channel, addr] (channel)}
								<div>
									<span class="text-[10px] uppercase tracking-wider text-muted-foreground">Channel: {channel}</span>
									<div class="flex items-center gap-1">
										<span class="text-xs font-mono text-foreground/80">
											{addr.net_id} / {addr.place_name}
										</span>
										<CopyButton text="{addr.net_id}/{addr.place_name}" />
									</div>
								</div>
							{/each}
						{/if}
					</div>
				</div>
			{/if}

			<!-- Token Data -->
			<div class="flex-1 flex flex-col min-h-0">
				<div class="shrink-0 flex items-center justify-between mb-2">
					<h3 class="text-sm font-medium text-foreground/80">Token Data</h3>
					{#if colorType !== 'Unit'}
						<CopyButton text={copyableData} />
					{/if}
				</div>
				{#if colorType === 'Unit'}
					<div class="px-3 py-6 bg-muted rounded-lg text-center text-muted-foreground italic">
						Unit token (no data)
					</div>
				{:else if colorType === 'Integer'}
					<div class="px-4 py-6 bg-muted rounded-lg text-center">
						<span class="text-3xl font-mono font-bold text-primary">{colorValue}</span>
					</div>
				{:else if colorType === 'Data'}
					<div class="flex-1 min-h-0">
						<MonacoEditor value={formattedData} language="json" height="100%" readOnly />
					</div>
				{/if}
			</div>
		</div>
	</div>
{/if}
