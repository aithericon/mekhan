<script lang="ts">
	import { NODE_PALETTE, type WorkflowNodeType } from '$lib/types/editor';
	import Play from '@lucide/svelte/icons/play';
	import Square from '@lucide/svelte/icons/square';
	import User from '@lucide/svelte/icons/user';
	import Cpu from '@lucide/svelte/icons/cpu';
	import GitBranch from '@lucide/svelte/icons/git-branch';
	import GitFork from '@lucide/svelte/icons/git-fork';
	import GitMerge from '@lucide/svelte/icons/git-merge';
	import Repeat from '@lucide/svelte/icons/repeat';
	import Flag from '@lucide/svelte/icons/flag';
	import Gauge from '@lucide/svelte/icons/gauge';
	import OctagonX from '@lucide/svelte/icons/octagon-x';

	const iconMap: Record<string, typeof Play> = {
		play: Play,
		square: Square,
		user: User,
		cpu: Cpu,
		'git-branch': GitBranch,
		'git-fork': GitFork,
		'git-merge': GitMerge,
		repeat: Repeat,
		flag: Flag,
		gauge: Gauge,
		'octagon-x': OctagonX
	};

	function onDragStart(event: DragEvent, nodeType: WorkflowNodeType) {
		if (!event.dataTransfer) return;
		event.dataTransfer.setData('application/mekhan-node-type', nodeType);
		event.dataTransfer.effectAllowed = 'move';
	}
</script>

<div class="flex w-56 flex-col border-r border-sidebar-border bg-sidebar" data-testid="node-palette">
	<div class="border-b border-sidebar-border px-3 py-2.5">
		<h2 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground">Blocks</h2>
	</div>
	<div class="flex-1 space-y-1 overflow-y-auto p-2">
		{#each NODE_PALETTE as item (item.type)}
			{@const Icon = iconMap[item.icon]}
			<!-- svelte-ignore a11y_no_static_element_interactions -->
			<div
				class="group flex cursor-grab items-center gap-2.5 rounded-lg border border-transparent px-2.5 py-2 text-sm transition-colors hover:border-border hover:bg-accent active:cursor-grabbing"
				draggable="true"
				data-testid="palette-item-{item.type}"
				ondragstart={(e) => onDragStart(e, item.type)}
			>
				<div
					class="flex size-7 shrink-0 items-center justify-center rounded-md"
					style="background-color: {item.color}20; color: {item.color};"
				>
					{#if Icon}
						<Icon class="size-4" />
					{/if}
				</div>
				<div class="min-w-0">
					<div class="text-sm font-medium text-foreground">{item.label}</div>
					<div class="truncate text-[10px] leading-tight text-muted-foreground">
						{item.description}
					</div>
				</div>
			</div>
		{/each}
	</div>
</div>
