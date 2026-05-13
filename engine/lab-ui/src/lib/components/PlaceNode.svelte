<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import { AlertCircle, AlertTriangle, ArrowUpRight, Reply, Inbox } from '@lucide/svelte';
	import * as Tooltip from '$lib/components/ui/tooltip';
	import type { Token } from '$lib/api/client';
	import type { IssueLevel } from '$lib/stores/lab.svelte';
	import TokenBadge from './TokenBadge.svelte';

	interface Props {
		data: {
			label: string;
			kind: string;
			tokens: Token[];
			bridgedOutTokens: Token[];
			bridgeTarget?: { target_net_id: string; target_place_name: string; reply_to?: string } | null;
			bridgeSource?: { source_net_id: string; source_place_name: string } | null;
			issueLevel?: IssueLevel | null;
			selected?: boolean;
			spotlightRole?: 'consumed' | 'produced' | 'target' | 'dimmed' | null;
			pulseRole?: 'appeared' | 'disappeared' | null;
			onSelect?: () => void;
			onSelectToken?: (tokenId: string) => void;
		};
	}

	let { data }: Props = $props();

	function handleClick(e: MouseEvent) {
		e.stopPropagation();
		data.onSelect?.();
	}

	const kindColors: Record<string, string> = {
		internal: 'border-blue-500 bg-blue-100 dark:bg-blue-950',
		signal: 'border-amber-500 bg-amber-100 dark:bg-amber-950',
		bridge_in: 'border-teal-500 bg-teal-100 dark:bg-teal-950',
		bridge_out: 'border-rose-500 bg-rose-100 dark:bg-rose-950',
		bridge_reply: 'border-indigo-500 bg-indigo-100 dark:bg-indigo-950'
	};

	const typeColor = $derived(kindColors[data.kind] || 'border-muted-foreground bg-muted');

	const bridgeTooltip = $derived.by(() => {
		if (data.kind === 'bridge_out' && data.bridgeTarget) {
			const target = `${data.bridgeTarget.target_net_id} / ${data.bridgeTarget.target_place_name}`;
			return data.bridgeTarget.reply_to
				? `Bridge to ${target} (reply: ${data.bridgeTarget.reply_to})`
				: `Bridge to ${target}`;
		}
		if (data.kind === 'bridge_reply') return 'Bridge reply inbox';
		if (data.kind === 'bridge_in') {
			if (data.bridgeSource) {
				return `Bridge from ${data.bridgeSource.source_net_id} / ${data.bridgeSource.source_place_name}`;
			}
			return 'Bridge inbox (receives from other nets)';
		}
		return '';
	});

	const isBridge = $derived(
		data.kind === 'bridge_out' || data.kind === 'bridge_reply' || data.kind === 'bridge_in'
	);
</script>

<div
	id="place-{data.label.toLowerCase().replace(/\s+/g, '-')}"
	data-testid="place-node"
	class="place-wrapper relative"
	onclick={handleClick}
	onkeydown={(e) => e.key === 'Enter' && handleClick(e as unknown as MouseEvent)}
	role="button"
	tabindex="0"
>
	<!-- Issue badge (absolute positioned) -->
	{#if data.issueLevel}
		<div class="absolute -top-1 -right-1 z-10">
			{#if data.issueLevel === 'error'}
				<div class="w-5 h-5 rounded-full bg-red-500 flex items-center justify-center shadow-sm">
					<AlertCircle class="w-3 h-3 text-white" />
				</div>
			{:else if data.issueLevel === 'warning'}
				<div class="w-5 h-5 rounded-full bg-amber-500 flex items-center justify-center shadow-sm">
					<AlertTriangle class="w-3 h-3 text-white" />
				</div>
			{/if}
		</div>
	{/if}

	<!-- Label header above the circle (absolute positioned) -->
	<span
		class="place-label absolute -top-4 left-1/2 -translate-x-1/2 text-[10px] font-semibold text-foreground text-center whitespace-nowrap max-w-28 truncate"
		title={data.label}
	>
		{data.label}
	</span>

	<!-- Circle body -->
	<div class="place-node rounded-full border-2 w-12 h-12 flex items-center justify-center cursor-pointer {data.selected ? 'ring-2 ring-primary' : 'hover:ring-2 hover:ring-blue-400'} {typeColor}
		{data.spotlightRole === 'consumed' ? 'spotlight-consumed' : ''}
		{data.spotlightRole === 'produced' ? 'spotlight-produced' : ''}
		{data.spotlightRole === 'target' ? 'spotlight-target' : ''}
		{data.spotlightRole === 'dimmed' ? 'spotlight-dimmed' : ''}
		{data.pulseRole === 'appeared' ? 'pulse-appeared' : ''}
		{data.pulseRole === 'disappeared' ? 'pulse-disappeared' : ''}
	">
		<Handle type="target" position={Position.Left} class="!bg-gray-400" />

		<div class="flex flex-wrap gap-0.5 justify-center items-center max-w-9 p-0.5">
			{#each data.tokens.slice(0, 5) as token (token.id)}
				<button class="token-click" onclick={(e) => { e.stopPropagation(); data.onSelectToken?.(token.id); }}>
					<TokenBadge {token} size="sm" />
				</button>
			{/each}
			{#each data.bridgedOutTokens.slice(0, Math.max(0, 5 - data.tokens.length)) as token (token.id)}
				<button class="token-click opacity-40" onclick={(e) => { e.stopPropagation(); data.onSelectToken?.(token.id); }}>
					<TokenBadge {token} size="sm" />
				</button>
			{/each}
			{#if data.tokens.length + data.bridgedOutTokens.length > 5}
				<span class="text-[8px] text-muted-foreground font-medium">
					+{data.tokens.length + data.bridgedOutTokens.length - 5}
				</span>
			{/if}
		</div>

		<Handle type="source" position={Position.Right} class="!bg-gray-400" />
	</div>

	<!-- Bridge badge below the circle -->
	{#if isBridge}
		<Tooltip.Root>
			<Tooltip.Trigger class="absolute -bottom-5 left-1/2 -translate-x-1/2 z-10">
				{#if data.kind === 'bridge_out'}
					<div class="bridge-badge flex items-center gap-0.5 px-1 py-0.5 text-[9px] font-mono font-semibold bg-rose-500/15 text-rose-700 dark:text-rose-400 border border-rose-500/30 rounded whitespace-nowrap">
						<ArrowUpRight class="w-2.5 h-2.5" />
						OUT
					</div>
				{:else if data.kind === 'bridge_in'}
					<div class="bridge-badge flex items-center gap-0.5 px-1 py-0.5 text-[9px] font-mono font-semibold bg-teal-500/15 text-teal-700 dark:text-teal-400 border border-teal-500/30 rounded whitespace-nowrap">
						<Inbox class="w-2.5 h-2.5" />
						IN
					</div>
				{:else}
					<div class="bridge-badge flex items-center gap-0.5 px-1 py-0.5 text-[9px] font-mono font-semibold bg-indigo-500/15 text-indigo-700 dark:text-indigo-400 border border-indigo-500/30 rounded whitespace-nowrap">
						<Reply class="w-2.5 h-2.5" />
						REPLY
					</div>
				{/if}
			</Tooltip.Trigger>
			<Tooltip.Content side="bottom" class="max-w-xs">
				<div class="text-xs">{bridgeTooltip}</div>
			</Tooltip.Content>
		</Tooltip.Root>
	{/if}
</div>

<style>
	.place-node {
		box-shadow: 0 1px 4px rgba(0, 0, 0, 0.08);
	}

	:global(.dark) .place-node {
		box-shadow: 0 2px 8px rgba(0, 0, 0, 0.25);
	}

	/* Spotlight glows */
	.spotlight-consumed {
		box-shadow: 0 0 0 3px rgba(239, 68, 68, 0.5), 0 0 14px rgba(239, 68, 68, 0.35) !important;
	}
	.spotlight-produced {
		box-shadow: 0 0 0 3px rgba(34, 197, 94, 0.5), 0 0 14px rgba(34, 197, 94, 0.35) !important;
	}
	.spotlight-target {
		box-shadow: 0 0 0 3px rgba(59, 130, 246, 0.5), 0 0 14px rgba(59, 130, 246, 0.35) !important;
	}
	.spotlight-dimmed {
		opacity: 0.2;
		transition: opacity 0.3s ease;
	}

	/* Pulse animations */
	@keyframes pulse-green {
		0% { box-shadow: 0 0 0 0 rgba(34, 197, 94, 0.7); }
		50% { box-shadow: 0 0 0 10px rgba(34, 197, 94, 0.25); }
		100% { box-shadow: 0 0 0 0 rgba(34, 197, 94, 0); }
	}
	@keyframes pulse-red {
		0% { box-shadow: 0 0 0 0 rgba(239, 68, 68, 0.7); }
		50% { box-shadow: 0 0 0 10px rgba(239, 68, 68, 0.25); }
		100% { box-shadow: 0 0 0 0 rgba(239, 68, 68, 0); }
	}
	.pulse-appeared {
		animation: pulse-green 0.6s ease-out;
	}
	.pulse-disappeared {
		animation: pulse-red 0.6s ease-out;
	}

	.bridge-badge {
		box-shadow: 0 1px 3px rgba(0, 0, 0, 0.1);
	}
</style>
