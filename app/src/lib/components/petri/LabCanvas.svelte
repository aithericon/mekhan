<script lang="ts">
	import {
		SvelteFlow,
		Background,
		Controls,
		MiniMap
	} from '@xyflow/svelte';
	import '@xyflow/svelte/dist/style.css';
	import { mode } from 'mode-watcher';
	import * as Tooltip from '$lib/components/ui/tooltip';
	import type {
		PetriNet,
		Token,
		TransitionStatus,
		ScenarioGroup,
		ValidationIssue,
		EventSpotlight,
		MarkingDiff
	} from '$lib/types/petri';
	import { topologyToFlow } from '$lib/petri/topology-to-flow';
	import PlaceNode from './PlaceNode.svelte';
	import TransitionNode from './TransitionNode.svelte';
	import GroupNode from './GroupNode.svelte';
	import MetaGroupNode from './MetaGroupNode.svelte';
	import RemoteNetNode from './RemoteNetNode.svelte';
	import CanvasController from './CanvasController.svelte';
	import NodeSearch from './NodeSearch.svelte';

	interface Props {
		presentationMode?: boolean;
		topology: PetriNet | null;
		marking: Map<string, Token[]>;
		bridgedOutTokens?: Map<string, Token[]>;
		enabledTransitions: string[];
		transitionStatuses: Record<string, TransitionStatus>;
		/** Current net ID (for spawn child lookup). */
		netId?: string;
		/** Spawn children grouped by parent net ID. */
		spawnChildren?: Map<string, { netId: string; label: string }[]>;
		/** Navigate to a child net's tab. */
		onNavigateToChild?: (netId: string) => void;
		issues?: ValidationIssue[];
		groups?: ScenarioGroup[];
		selectedElementId?: string | null;
		spotlight?: EventSpotlight | null;
		markingDiff?: MarkingDiff | null;
		onFireTransition: (transitionId: string) => void;
		onSelectPlace?: (placeId: string) => void;
		onSelectTransition?: (transitionId: string) => void;
		onSelectToken?: (placeId: string, tokenId: string) => void;
		onSelectGroup?: (groupId: string) => void;
		onSelectRemoteNet?: (id: string, label: string, targets: string[], sources: string[], childNetIds: string[]) => void;
	}

	let { presentationMode = false, topology, marking, bridgedOutTokens, enabledTransitions, transitionStatuses, issues = [], groups = [], selectedElementId = null, spotlight = null, markingDiff = null, netId, spawnChildren, onNavigateToChild, onFireTransition, onSelectPlace, onSelectTransition, onSelectToken, onSelectGroup, onSelectRemoteNet }: Props = $props();

	let showCausation = $state(true);
	let showBridges = $state(true);
	let showReadArcs = $state(true);
	let collapseGroups = $state(false);

	const nodeTypes = {
		place: PlaceNode,
		transition: TransitionNode,
		group: GroupNode,
		metagroup: MetaGroupNode,
		remotenet: RemoteNetNode
	};

	// Topology → positioned flow graph. All of the (pure) transform logic now
	// lives in lib/petri; this component is just props + toggle UI + <SvelteFlow>.
	const { nodes, edges } = $derived.by(() =>
		topologyToFlow({
			topology,
			marking,
			bridgedOutTokens,
			enabledTransitions,
			transitionStatuses,
			issues,
			groups,
			selectedElementId,
			spotlight,
			markingDiff,
			showCausation,
			showBridges,
			showReadArcs,
			collapseGroups,
			netId,
			spawnChildren,
			onNavigateToChild,
			onFireTransition,
			onSelectPlace,
			onSelectTransition,
			onSelectToken,
			onSelectGroup,
			onSelectRemoteNet
		})
	);
</script>

<div id="lab-canvas" class="lab-canvas w-full h-full relative">
	<SvelteFlow {nodes} {edges} {nodeTypes} fitView colorMode={mode.current ?? 'system'} minZoom={0.05}>
		<CanvasController {spotlight} />
		<Background />
		{#if !presentationMode}
		<NodeSearch {topology} {onSelectPlace} {onSelectTransition} />
		<Controls />
		<MiniMap
			nodeColor={(node) => {
				if (node.type === 'place') return 'hsl(211 49% 65%)';
				if (node.type === 'transition') return 'hsl(215 15% 45%)';
				if (node.type === 'metagroup') return 'hsl(211 49% 55%)';
				if (node.type === 'remotenet') return 'hsl(168 50% 55%)';
				return 'transparent';
			}}
			maskStrokeColor="hsl(211 49% 60%)"
			maskStrokeWidth={2}
		/>
		{/if}
	</SvelteFlow>

	<!-- Canvas overlay toggles -->
	{#if !presentationMode}
		<div class="absolute bottom-2 left-14 z-10 flex items-center gap-1.5">
			<Tooltip.Root>
				<Tooltip.Trigger>
					<button
						class="canvas-toggle flex items-center gap-1.5 px-2 py-1.5 rounded-md text-sm font-medium border transition-colors
							{showCausation
								? 'bg-orange-50 border-orange-300 text-orange-700 hover:bg-orange-100 dark:bg-orange-950 dark:border-orange-700 dark:text-orange-300 dark:hover:bg-orange-900'
								: 'bg-card border-border text-muted-foreground hover:bg-accent'}"
						onclick={() => (showCausation = !showCausation)}
					>
						<svg class="w-3.5 h-3.5" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5">
							<path d="M2 8h8" stroke-dasharray="3,2" />
							<path d="M11 5l3 3-3 3" />
						</svg>
						Causes
					</button>
				</Tooltip.Trigger>
				<Tooltip.Content side="top">
					<span class="text-sm">{showCausation ? 'Hide' : 'Show'} causation arcs</span>
				</Tooltip.Content>
			</Tooltip.Root>

			<Tooltip.Root>
				<Tooltip.Trigger>
					<button
						class="canvas-toggle flex items-center gap-1.5 px-2 py-1.5 rounded-md text-sm font-medium border transition-colors
							{showBridges
								? 'bg-teal-50 border-teal-300 text-teal-700 hover:bg-teal-100 dark:bg-teal-950 dark:border-teal-700 dark:text-teal-300 dark:hover:bg-teal-900'
								: 'bg-card border-border text-muted-foreground hover:bg-accent'}"
						onclick={() => (showBridges = !showBridges)}
					>
						<svg class="w-3.5 h-3.5" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5">
							<path d="M2 8h4" stroke-dasharray="2,2" />
							<rect x="6" y="5" width="4" height="6" rx="1" />
							<path d="M10 8h4" stroke-dasharray="2,2" />
						</svg>
						Bridges
					</button>
				</Tooltip.Trigger>
				<Tooltip.Content side="top">
					<span class="text-sm">{showBridges ? 'Hide' : 'Show'} bridge connections</span>
				</Tooltip.Content>
			</Tooltip.Root>

			<Tooltip.Root>
				<Tooltip.Trigger>
					<button
						class="canvas-toggle flex items-center gap-1.5 px-2 py-1.5 rounded-md text-sm font-medium border transition-colors
							{showReadArcs
								? 'bg-violet-50 border-violet-300 text-violet-700 hover:bg-violet-100 dark:bg-violet-950 dark:border-violet-700 dark:text-violet-300 dark:hover:bg-violet-900'
								: 'bg-card border-border text-muted-foreground hover:bg-accent'}"
						onclick={() => (showReadArcs = !showReadArcs)}
					>
						<svg class="w-3.5 h-3.5" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5">
							<path d="M2 8h8" stroke-dasharray="4,3" />
							<path d="M11 5l3 3-3 3" />
						</svg>
						Read
					</button>
				</Tooltip.Trigger>
				<Tooltip.Content side="top">
					<span class="text-sm">{showReadArcs ? 'Hide' : 'Show'} read arcs</span>
				</Tooltip.Content>
			</Tooltip.Root>

			{#if groups.length > 0}
				<Tooltip.Root>
					<Tooltip.Trigger>
						<button
							class="canvas-toggle flex items-center gap-1.5 px-2 py-1.5 rounded-md text-sm font-medium border transition-colors
								{collapseGroups
									? 'bg-sky-50 border-sky-300 text-sky-700 hover:bg-sky-100 dark:bg-sky-950 dark:border-sky-700 dark:text-sky-300 dark:hover:bg-sky-900'
									: 'bg-card border-border text-muted-foreground hover:bg-accent'}"
							onclick={() => (collapseGroups = !collapseGroups)}
						>
							<svg class="w-3.5 h-3.5" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5">
								<rect x="1" y="1" width="6" height="6" rx="1" />
								<rect x="9" y="1" width="6" height="6" rx="1" />
								<rect x="5" y="9" width="6" height="6" rx="1" />
							</svg>
							Collapse
						</button>
					</Tooltip.Trigger>
					<Tooltip.Content side="top">
						<span class="text-sm">{collapseGroups ? 'Expand' : 'Collapse'} groups into summary nodes</span>
					</Tooltip.Content>
				</Tooltip.Root>
			{/if}
		</div>
	{/if}
</div>

<style>
	.canvas-toggle {
		box-shadow: 0 1px 4px rgba(0, 0, 0, 0.1);
	}

	.lab-canvas :global(.svelte-flow) {
		background-color: var(--background);

		/* Override SvelteFlow variables to match our theme */
		--xy-background-color: var(--background);
		--xy-background-pattern-dots-color: var(--flow-dots);

		/* Controls */
		--xy-controls-button-background-color: var(--card);
		--xy-controls-button-background-color-hover: var(--accent);
		--xy-controls-button-color: var(--foreground);
		--xy-controls-button-color-hover: var(--foreground);
		--xy-controls-button-border-color: var(--border);
		--xy-controls-box-shadow: 0 0 2px 1px rgba(0, 0, 0, 0.08);

		/* Minimap */
		--xy-minimap-background-color: var(--card);
		--xy-minimap-mask-background-color: var(--flow-minimap-mask);
		--xy-minimap-node-background-color: var(--muted-foreground);
		--xy-minimap-node-stroke-color: transparent;

		/* Edges */
		--xy-edge-stroke: var(--flow-edge);
		--xy-edge-stroke-selected: var(--primary);

		/* Edge labels */
		--xy-edge-label-background-color: var(--card);
		--xy-edge-label-color: var(--foreground);

		/* Attribution */
		--xy-attribution-background-color: color-mix(in srgb, var(--foreground) 15%, transparent);
	}
</style>
