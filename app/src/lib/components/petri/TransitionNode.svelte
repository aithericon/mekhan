<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import { Play, AlertCircle, AlertTriangle, Zap } from '@lucide/svelte';
	import * as Tooltip from '$lib/components/ui/tooltip';
	import type { Port, TransitionStatus, IssueLevel } from '$lib/types/petri';

	interface CausedSignal {
		id: string;
		name: string;
	}

	interface Props {
		data: {
			label: string;
			enabled: boolean;
			inputPorts: Port[];
			outputPorts: Port[];
			causedSignals?: CausedSignal[];
			guard?: string | null;
			script: string;
			logicType?: 'rhai' | 'wasm' | 'effect';
			handlerId?: string | null;
			status?: TransitionStatus;
			issueLevel?: IssueLevel | null;
			selected?: boolean;
			spotlightRole?: 'fired' | 'dimmed' | null;
			pulseRole?: 'fired' | null;
			onFire: () => void;
			onSelect?: () => void;
			/** Width/height predicted by topology-to-flow + getTransitionWidth.
			 *  Pinning the chip to this width is what keeps dagre's layout in
			 *  sync with the rendered DOM (no more long-label spillover). */
			_dims?: { width: number; height: number };
		};
	}

	let { data }: Props = $props();
	const chipWidth = $derived(data._dims?.width ?? 200);

	// Derive disabled reason from status
	const disabledReason = $derived.by(() => {
		if (!data.status || data.status === 'enabled') return null;

		switch (data.status) {
			case 'disabled_no_tokens':
				return `Missing tokens at place`;
			case 'disabled_guard_failed':
				return `Guard failed`;
			case 'disabled_guard_error':
				return `Guard error`;
			default:
				return 'Disabled';
		}
	});

	function handleClick(e: MouseEvent) {
		// Click on body just opens the inspector
		data.onSelect?.();
	}

	function handleFire(e: MouseEvent) {
		e.stopPropagation(); // Don't trigger select
		if (data.enabled) {
			data.onFire();
		}
	}

	const hasGuard = $derived(!!data.guard);
	const isEffect = $derived(data.logicType === 'effect');
	const hasCausedSignals = $derived((data.causedSignals?.length || 0) > 0);
	const hasMultiplePorts = $derived(
		(data.inputPorts?.length || 0) > 1 || (data.outputPorts?.length || 0) > 1 || hasCausedSignals
	);
</script>

<div class="transition-wrapper relative">
	<!-- Issue badge (absolute positioned) -->
	{#if data.issueLevel}
		<div class="absolute -top-2 -right-2 z-10">
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

	<Tooltip.Root>
		<Tooltip.Trigger>
			<div
				id="transition-{data.label.toLowerCase().replace(/\s+/g, '-')}"
				data-testid="transition-node"
				class="transition-chip flex flex-col border-2 rounded-lg cursor-pointer bg-card bg-linear-to-br
					{hasGuard ? 'border-amber-500' : ''}
					{isEffect && data.enabled ? 'border-purple-400 from-purple-500/10 to-purple-500/25 hover:from-purple-500/15 hover:to-purple-500/30' : ''}
					{isEffect && !data.enabled ? 'border-purple-300 from-purple-500/5 to-purple-500/15 hover:from-purple-500/10 hover:to-purple-500/20' : ''}
					{!isEffect && data.enabled
						? 'border-gray-400 from-gray-500/5 to-gray-500/20 hover:from-gray-500/10 hover:to-gray-500/25 dark:border-gray-600'
						: ''}
					{!isEffect && !data.enabled
						? 'border-gray-300 from-gray-500/5 to-gray-500/10 hover:from-gray-500/10 hover:to-gray-500/15 dark:border-gray-600/60'
						: ''}
					{data.selected ? 'ring-2 ring-primary ring-offset-1 ring-offset-background' : ''}
					{data.spotlightRole === 'fired' ? 'spotlight-fired' : ''}
					{data.spotlightRole === 'dimmed' ? 'spotlight-dimmed' : ''}
					{data.pulseRole === 'fired' ? 'pulse-fired' : ''}"
				style="width: {chipWidth}px;"
				onclick={handleClick}
				onkeydown={(e) => e.key === 'Enter' && handleClick(e as unknown as MouseEvent)}
				role="button"
				tabindex="0"
			>
				<!-- Header: Transition name + badges + fire button -->
				<div class="flex items-center px-2 py-1 min-w-0 gap-1">
					<span
						class="text-sm font-semibold truncate mr-auto min-w-0
						{data.enabled ? 'text-gray-900 dark:text-white' : 'text-gray-600 dark:text-gray-400'}"
					>
						{data.label}
					</span>
					{#if hasGuard}
						<Tooltip.Root>
							<Tooltip.Trigger>
								<div
									class="guard-badge shrink-0 px-1.5 py-0.5 text-sm font-mono font-semibold bg-amber-100 text-amber-900 dark:bg-amber-900 dark:text-amber-200 border border-amber-300 dark:border-amber-700 rounded whitespace-nowrap"
								>
									G
								</div>
							</Tooltip.Trigger>
							<Tooltip.Content side="top" class="max-w-xs">
								<div class="text-sm font-mono">{data.guard}</div>
							</Tooltip.Content>
						</Tooltip.Root>
					{/if}
					{#if isEffect}
						<Tooltip.Root>
							<Tooltip.Trigger>
								<div
									class="effect-badge shrink-0 flex items-center gap-0.5 px-1.5 py-0.5 text-sm font-mono font-semibold bg-purple-100 text-purple-900 dark:bg-purple-800 dark:text-purple-200 border border-purple-300 dark:border-purple-600 rounded whitespace-nowrap"
								>
									<Zap class="w-2.5 h-2.5" />
									FX
								</div>
							</Tooltip.Trigger>
							<Tooltip.Content side="top" class="max-w-xs">
								<div class="text-sm font-mono">Effect: {data.handlerId ?? 'unknown'}</div>
							</Tooltip.Content>
						</Tooltip.Root>
					{/if}
					{#if data.enabled}
						<button
							class="fire-btn shrink-0 p-0.5 rounded bg-green-500 hover:bg-green-400 transition-colors"
							onclick={handleFire}
							aria-label="Fire transition"
						>
							<Play class="w-3 h-3 text-white" fill="white" />
						</button>
					{/if}
				</div>

				<!-- Ports row -->
				<div class="flex border-t border-border">
					<!-- Input ports -->
					<div class="input-ports flex flex-col justify-center items-start relative px-1 py-1">
						{#if data.inputPorts && data.inputPorts.length > 0}
							{#each data.inputPorts as port (port.name)}
								<div
									class="port-row flex items-center gap-1"
									style="position: relative;"
								>
									<Handle
										type="target"
										position={Position.Left}
										id={port.name}
										class="!bg-blue-400 !w-2 !h-2"
										style="position: relative;"
									/>
									{#if hasMultiplePorts}
										<span
											class="port-label text-sm font-mono whitespace-nowrap
											{data.enabled ? 'text-gray-600 dark:text-gray-200' : 'text-gray-500 dark:text-gray-400'}"
										>
											{port.name}
										</span>
									{/if}
								</div>
							{/each}
						{:else}
							<Handle type="target" position={Position.Left} class="!bg-gray-400" />
						{/if}
					</div>

					<div class="flex-1"></div>

					<!-- Output ports -->
					<div class="output-ports flex flex-col justify-center items-end relative px-1 py-1">
						{#if data.outputPorts && data.outputPorts.length > 0}
							{#each data.outputPorts as port (port.name)}
								<div
									class="port-row flex items-center gap-1"
									style="position: relative;"
								>
									{#if hasMultiplePorts}
										<span
											class="port-label text-sm font-mono whitespace-nowrap
											{port.name === '_error' ? 'text-red-400/70' : data.enabled ? 'text-gray-600 dark:text-gray-200' : 'text-gray-500 dark:text-gray-400'}"
										>
											{port.name}
										</span>
									{/if}
									<Handle
										type="source"
										position={Position.Right}
										id={port.name}
										class="!bg-green-400 !w-2 !h-2"
										style="position: relative;"
									/>
								</div>
							{/each}
						{:else}
							<Handle type="source" position={Position.Right} class="!bg-gray-400" />
						{/if}
						{#if hasCausedSignals}
							<div class="causation-divider w-full border-t border-orange-300 dark:border-orange-600 my-0.5"></div>
							{#each data.causedSignals as sig (sig.id)}
								<div
									class="port-row flex items-center gap-1"
									style="position: relative;"
								>
									<span
										class="port-label text-sm font-mono whitespace-nowrap text-orange-500 dark:text-orange-400"
									>
										{sig.name}
									</span>
									<Handle
										type="source"
										position={Position.Right}
										id={`cause-${sig.id}`}
										class="!bg-orange-400 !w-2 !h-2"
										style="position: relative;"
									/>
								</div>
							{/each}
						{/if}
					</div>
				</div>
			</div>
		</Tooltip.Trigger>
		<Tooltip.Content side="bottom" class="max-w-sm">
			<div class="text-sm">
				<span class="font-medium">{data.label}</span>
				{#if data.enabled}
					<div class="mt-1 text-green-600">Click to inspect, or press play to fire</div>
				{:else}
					{#if disabledReason}
						<div class="mt-1 text-amber-600">{disabledReason}</div>
					{:else}
						<div class="mt-1 text-gray-500">Not enabled</div>
					{/if}
				{/if}
			</div>
			{#if isEffect}
				<div class="mt-1 text-sm font-mono text-purple-600">
					Effect: {data.handlerId ?? 'unknown'}
				</div>
			{:else if data.script}
				<div class="mt-1 text-sm font-mono text-gray-500 truncate">
					Script: {data.script.slice(0, 40)}...
				</div>
			{/if}
		</Tooltip.Content>
	</Tooltip.Root>
</div>

<style>
	.transition-wrapper {
		padding-top: 0;
	}

	.transition-chip {
		box-shadow: 0 1px 4px rgba(0, 0, 0, 0.08);
		min-height: 40px;
	}

	:global(.dark) .transition-chip {
		box-shadow: 0 2px 8px rgba(0, 0, 0, 0.3);
	}

	.transition-chip:hover:not(:disabled) {
		transform: scale(1.02);
		transition: transform 0.1s ease;
	}

	.guard-badge,
	.effect-badge {
		box-shadow: 0 1px 3px rgba(0, 0, 0, 0.1);
	}

	.port-row {
		min-height: 22px;
	}

	.input-ports,
	.output-ports {
		min-width: 8px;
	}

	/* Spotlight glows */
	.spotlight-fired {
		box-shadow: 0 0 0 3px rgba(245, 158, 11, 0.5), 0 0 16px rgba(245, 158, 11, 0.4) !important;
	}
	.spotlight-dimmed {
		opacity: 0.2;
		transition: opacity 0.3s ease;
	}

	/* Pulse animation */
	@keyframes pulse-amber {
		0% { box-shadow: 0 0 0 0 rgba(245, 158, 11, 0.7); }
		50% { box-shadow: 0 0 0 10px rgba(245, 158, 11, 0.25); }
		100% { box-shadow: 0 0 0 0 rgba(245, 158, 11, 0); }
	}
	.pulse-fired {
		animation: pulse-amber 0.6s ease-out;
	}
</style>
