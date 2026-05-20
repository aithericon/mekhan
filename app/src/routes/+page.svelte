<script lang="ts">
	import { goto } from '$app/navigation';
	import { Button } from '$lib/components/ui/button';
	import { findOrCreateShowcaseTemplate } from '$lib/templates/showcase';
	import Play from '@lucide/svelte/icons/play';
	import Square from '@lucide/svelte/icons/square';
	import User from '@lucide/svelte/icons/user';
	import Cpu from '@lucide/svelte/icons/cpu';
	import GitBranch from '@lucide/svelte/icons/git-branch';
	import GitFork from '@lucide/svelte/icons/git-fork';
	import GitMerge from '@lucide/svelte/icons/git-merge';
	import Repeat from '@lucide/svelte/icons/repeat';
	import Rocket from '@lucide/svelte/icons/rocket';
	import Layers from '@lucide/svelte/icons/layers';
	import Activity from '@lucide/svelte/icons/activity';
	import Zap from '@lucide/svelte/icons/zap';

	let openingDemo = $state(false);
	let demoError = $state<string | null>(null);

	async function openDemo() {
		if (openingDemo) return;
		openingDemo = true;
		demoError = null;
		try {
			const template = await findOrCreateShowcaseTemplate();
			await goto(`/templates/${template.id}`);
		} catch (e) {
			demoError = e instanceof Error ? e.message : 'Failed to open demo. Is mekhan-service running?';
		} finally {
			openingDemo = false;
		}
	}

	const nodeTypes = [
		{ icon: Play, label: 'Start', color: '#22c55e' },
		{ icon: Square, label: 'End', color: '#ef4444' },
		{ icon: User, label: 'Human Task', color: '#3b82f6' },
		{ icon: Cpu, label: 'Automated', color: '#8b5cf6' },
		{ icon: GitBranch, label: 'Decision', color: '#f59e0b' },
		{ icon: GitFork, label: 'Split', color: '#06b6d4' },
		{ icon: GitMerge, label: 'Join', color: '#06b6d4' },
		{ icon: Repeat, label: 'Loop', color: '#ec4899' }
	];
</script>

<div class="flex h-full items-center justify-center" data-testid="home-page">
	<div class="w-full max-w-2xl px-6 animate-rise">
		<div class="text-center">
			<h1 class="text-3xl font-semibold tracking-tight text-foreground">Mekhan</h1>
			<p class="mt-2 text-sm text-muted-foreground">Visual workflow editor for Petri-Lab</p>
			<div class="mt-6 flex items-center justify-center gap-3">
				<Button data-testid="btn-try-demo" disabled={openingDemo} onclick={openDemo}>
					<Rocket class="size-4" />
					{openingDemo ? 'Opening…' : 'Try Demo'}
				</Button>
				<Button variant="outline" href="/templates" data-testid="btn-view-templates">
					Templates
				</Button>
				<Button variant="outline" href="/instances" data-testid="btn-view-instances">
					Instances
				</Button>
			</div>
			{#if demoError}
				<p class="mt-3 text-sm text-amber-700" data-testid="demo-error">{demoError}</p>
			{/if}
		</div>

		<!-- Feature cards -->
		<div class="mt-12 grid grid-cols-3 gap-4">
			<!-- Node Types -->
			<div class="rounded-xl border border-border bg-card p-4">
				<div class="mb-3 flex items-center gap-2">
					<div class="flex size-7 items-center justify-center rounded-lg bg-primary/10">
						<Layers class="size-4 text-primary" />
					</div>
					<span class="text-sm font-medium text-foreground">8 Block Types</span>
				</div>
				<div class="grid grid-cols-4 gap-1.5">
					{#each nodeTypes as nt (nt.label)}
						<div class="flex flex-col items-center gap-1 rounded-lg border border-border/50 p-1.5">
							<div
								class="flex size-6 items-center justify-center rounded-md"
								style="background-color: {nt.color}20; color: {nt.color};"
							>
								<nt.icon class="size-3.5" />
							</div>
							<span class="text-sm leading-none text-muted-foreground">{nt.label}</span>
						</div>
					{/each}
				</div>
			</div>

			<!-- Execution -->
			<div class="rounded-xl border border-border bg-card p-4">
				<div class="mb-3 flex items-center gap-2">
					<div class="flex size-7 items-center justify-center rounded-lg bg-primary/10">
						<Zap class="size-4 text-primary" />
					</div>
					<span class="text-sm font-medium text-foreground">Live Execution</span>
				</div>
				<div class="space-y-2 text-sm text-muted-foreground">
					<div class="flex items-center gap-2">
						<div class="size-1.5 rounded-full bg-green-500"></div>
						<span>Petri-Lab engine (Colored Petri Nets)</span>
					</div>
					<div class="flex items-center gap-2">
						<div class="size-1.5 rounded-full bg-blue-500"></div>
						<span>NATS JetStream event sourcing</span>
					</div>
					<div class="flex items-center gap-2">
						<div class="size-1.5 rounded-full bg-violet-500"></div>
						<span>Python / Docker / Process backends</span>
					</div>
					<div class="flex items-center gap-2">
						<div class="size-1.5 rounded-full bg-cyan-500"></div>
						<span>Human-in-the-loop tasks</span>
					</div>
				</div>
			</div>

			<!-- Workflow -->
			<div class="rounded-xl border border-border bg-card p-4">
				<div class="mb-3 flex items-center gap-2">
					<div class="flex size-7 items-center justify-center rounded-lg bg-primary/10">
						<Activity class="size-4 text-primary" />
					</div>
					<span class="text-sm font-medium text-foreground">Full Lifecycle</span>
				</div>
				<div class="space-y-2 text-sm text-muted-foreground">
					<div class="flex items-center gap-2">
						<span class="flex size-4 items-center justify-center rounded bg-muted text-sm font-semibold text-muted-foreground">1</span>
						<span>Design with drag-and-drop editor</span>
					</div>
					<div class="flex items-center gap-2">
						<span class="flex size-4 items-center justify-center rounded bg-muted text-sm font-semibold text-muted-foreground">2</span>
						<span>Compile to AIR and publish</span>
					</div>
					<div class="flex items-center gap-2">
						<span class="flex size-4 items-center justify-center rounded bg-muted text-sm font-semibold text-muted-foreground">3</span>
						<span>Deploy to Petri-Lab for execution</span>
					</div>
					<div class="flex items-center gap-2">
						<span class="flex size-4 items-center justify-center rounded bg-muted text-sm font-semibold text-muted-foreground">4</span>
						<span>Monitor live state and transitions</span>
					</div>
				</div>
			</div>
		</div>
	</div>
</div>
