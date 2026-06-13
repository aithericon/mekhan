<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import { StatusBadge } from '$lib/components/status';
	import {
		listInstances,
		listTemplates,
		listTasks,
		listTaskInbox,
		createTemplate,
		type InstanceListItem,
		type TemplateSummary
	} from '$lib/api/client';
	import type { HumanTask } from '$lib/types/tasks';
	import { findShowcaseTemplate } from '$lib/templates/showcase';
	import BrandSpiral from '$lib/components/BrandSpiral.svelte';
	import BrandCorners from '$lib/components/BrandCorners.svelte';
	import { auth } from '$lib/auth/store.svelte';
	import Rocket from '@lucide/svelte/icons/rocket';
	import Plus from '@lucide/svelte/icons/plus';
	import Activity from '@lucide/svelte/icons/activity';
	import ClipboardList from '@lucide/svelte/icons/clipboard-list';
	import FileText from '@lucide/svelte/icons/file-text';
	import CheckCircle2 from '@lucide/svelte/icons/check-circle-2';
	import ArrowRight from '@lucide/svelte/icons/arrow-right';
	import Hand from '@lucide/svelte/icons/hand';
	import Inbox from '@lucide/svelte/icons/inbox';

	let openingDemo = $state(false);
	let demoError = $state<string | null>(null);

	let loading = $state(true);
	let loadError = $state<string | null>(null);
	let runningInstances = $state<InstanceListItem[]>([]);
	let recentInstances = $state<InstanceListItem[]>([]);
	let recentTemplates = $state<TemplateSummary[]>([]);
	let inboxTasks = $state<HumanTask[]>([]);

	// Inbox preview: work I can take on — pooled offers (caps-gated) AND unpooled
	// "open to anyone" tasks (status `pending`) — vs. the work I've already
	// claimed (my open work). Sorted newest-first, capped. A task lives in exactly
	// one bucket: pending/offered are claimable, claimed is mine.
	const offeredTasks = $derived(
		inboxTasks
			.filter((t) => t.status === 'offered' || t.status === 'pending')
			.sort((a, b) => new Date(b.created_at).getTime() - new Date(a.created_at).getTime())
	);
	const myOpenTasks = $derived(
		inboxTasks
			.filter((t) => t.status === 'claimed')
			.sort((a, b) => new Date(b.created_at).getTime() - new Date(a.created_at).getTime())
	);
	const hasInboxPreview = $derived(offeredTasks.length > 0 || myOpenTasks.length > 0);
	let stats = $state({
		running: 0,
		pendingTasks: 0,
		templates: 0,
		draftTemplates: 0,
		completedToday: 0,
		failedToday: 0
	});

	async function openDemo() {
		if (openingDemo) return;
		openingDemo = true;
		demoError = null;
		try {
			const template = await findShowcaseTemplate();
			if (!template) {
				// Seeded by the service at startup, gated by
				// `MEKHAN__DEMOS__SEED`. Direct the user to the toggle
				// rather than silently doing nothing.
				demoError =
					'Demo not seeded yet. Restart mekhan-service with MEKHAN__DEMOS__SEED=true (or run `just dev::up`).';
				return;
			}
			await goto(`/templates/${template.id}`);
		} catch (e) {
			demoError = e instanceof Error ? e.message : 'Failed to open demo. Is mekhan-service running?';
		} finally {
			openingDemo = false;
		}
	}

	let creatingTemplate = $state(false);
	async function newTemplate() {
		if (creatingTemplate) return;
		creatingTemplate = true;
		try {
			const t = await createTemplate({ name: 'Untitled Workflow', description: '' });
			await goto(`/templates/${t.id}`);
		} catch {
			// Fall back to the templates index — it handles "create" too and
			// surfaces any underlying API issue with its own error banner.
			await goto('/templates');
		} finally {
			creatingTemplate = false;
		}
	}

	async function loadDashboard() {
		loading = true;
		loadError = null;
		try {
			// Two passes of completed/failed are scoped to today only — pull a
			// modest page (cheap) and filter client-side. The bigger lists keep
			// `perPage:1` to fetch just the `total` for the stat tiles.
			const sinceMidnight = new Date();
			sinceMidnight.setHours(0, 0, 0, 0);

			const [running, completedRecent, failedRecent, templates, pendingTasks, inbox] =
				await Promise.all([
					listInstances({ status: 'running', perPage: 6 }),
					listInstances({ status: 'completed', perPage: 20 }),
					listInstances({ status: 'failed', perPage: 20 }),
					listTemplates({ pageSize: 50 }),
					listTasks({ status: 'pending', limit: 1 }),
					// The inbox is a per-user surface; a failure here (e.g. no human
					// capacity in this workspace) shouldn't blank the whole dashboard.
					listTaskInbox().catch(() => ({ tasks: [] as HumanTask[] }))
				]);

			runningInstances = running.items;
			inboxTasks = inbox.tasks;
			recentInstances =
				running.items.length >= 5 ? running.items.slice(0, 5) : [
					...running.items,
					...completedRecent.items.slice(0, 5 - running.items.length)
				];
			recentTemplates = templates.items.slice(0, 5);

			stats = {
				running: running.total,
				pendingTasks: pendingTasks.total,
				templates: templates.total,
				draftTemplates: templates.items.filter((t) => !t.published).length,
				completedToday: completedRecent.items.filter(
					(i) => new Date(i.created_at).getTime() >= sinceMidnight.getTime()
				).length,
				failedToday: failedRecent.items.filter(
					(i) => new Date(i.created_at).getTime() >= sinceMidnight.getTime()
				).length
			};
		} catch (e) {
			loadError = e instanceof Error ? e.message : 'Failed to load dashboard';
		} finally {
			loading = false;
		}
	}

	onMount(loadDashboard);

	function formatRelative(s: string): string {
		const then = new Date(s).getTime();
		const diff = Math.max(0, Date.now() - then);
		const min = Math.floor(diff / 60000);
		if (min < 1) return 'just now';
		if (min < 60) return `${min}m ago`;
		const hr = Math.floor(min / 60);
		if (hr < 24) return `${hr}h ago`;
		const d = Math.floor(hr / 24);
		return `${d}d ago`;
	}

	const displayName = $derived(
		auth.session?.user.displayName?.split(' ')[0] ??
			auth.session?.user.email?.split('@')[0] ??
			null
	);

	const isEmpty = $derived(
		!loading && stats.templates === 0 && stats.running === 0
	);
</script>

<svelte:head>
	<title>Mekhan</title>
</svelte:head>

<div class="relative h-full overflow-hidden" data-testid="home-page">
	<!-- Abstract brand motifs: spiral top-right (primary), nested corners
	     bottom-left (warm accent) — the two accents bracketing the panel.
	     They live OUTSIDE the scroll layer, pinned to the panel corners and
	     clipped by it: an abspos child overhanging a scroll container's
	     bottom edge would otherwise become scrollable overflow (phantom
	     whitespace below the content). -->
	<BrandSpiral
		class="pointer-events-none absolute -top-16 -right-16 z-0 size-[22rem] text-primary opacity-[0.10] select-none dark:opacity-[0.16]"
	/>
	<BrandCorners
		class="pointer-events-none absolute -bottom-28 -left-28 z-0 size-[32rem] text-accent-warm opacity-[0.35] select-none dark:opacity-[0.30]"
	/>
	<div class="relative z-10 h-full overflow-x-hidden overflow-y-auto">
		<div class="mx-auto max-w-6xl px-6 py-10 animate-rise">
		<!-- Header: greeting + primary CTAs ----------------------------- -->
		<div class="flex flex-wrap items-end justify-between gap-4">
			<div>
				<h1
					class="text-4xl font-semibold tracking-tight text-foreground"
					style="font-family: 'Fraunces', serif;"
				>
					{displayName ? `Welcome back, ${displayName}` : 'Mekhan'}
				</h1>
				<p class="mt-1.5 text-sm text-muted-foreground">
					Visual workflow editor for Petri-Lab — design, publish, run.
				</p>
			</div>
			<div class="flex items-center gap-2">
				<Button
					variant="warm"
					data-testid="btn-try-demo"
					disabled={openingDemo}
					onclick={openDemo}
				>
					<Rocket class="size-4" />
					{openingDemo ? 'Opening…' : 'Open Demo'}
				</Button>
				<Button data-testid="btn-new-template" disabled={creatingTemplate} onclick={newTemplate}>
					<Plus class="size-4" />
					{creatingTemplate ? 'Creating…' : 'New Template'}
				</Button>
			</div>
		</div>

		{#if demoError}
			<p
				class="mt-3 rounded-md border border-amber-200 bg-amber-50 px-3 py-2 text-sm text-amber-800"
				data-testid="demo-error"
			>
				{demoError}
			</p>
		{/if}

		{#if loadError}
			<p class="mt-3 rounded-md border border-amber-200 bg-amber-50 px-3 py-2 text-sm text-amber-800">
				{loadError}
			</p>
		{/if}

		<!-- Stats row ---------------------------------------------------- -->
		<div class="mt-8 grid grid-cols-2 gap-3 sm:grid-cols-4">
			<a
				href="/instances?status=running"
				data-testid="btn-view-instances"
				class="group rounded-xl border border-transparent bg-accent-warm p-4 transition-colors hover:bg-accent-warm/90"
			>
				<div class="flex items-center justify-between">
					<span class="text-sm font-medium uppercase tracking-wider text-accent-warm-foreground/70">
						Running
					</span>
					<Activity class="size-4 text-accent-warm-foreground/60" />
				</div>
				<div class="mt-2 flex items-baseline gap-1.5">
					<span class="text-3xl font-semibold tabular-nums text-accent-warm-foreground">{loading ? '—' : stats.running}</span>
					<span class="text-sm text-accent-warm-foreground/70">
						{stats.running === 1 ? 'instance' : 'instances'}
					</span>
				</div>
			</a>

			<a
				href="/tasks"
				class="group rounded-xl border border-border bg-card p-4 transition-colors hover:bg-accent/50"
			>
				<div class="flex items-center justify-between">
					<span class="text-sm font-medium uppercase tracking-wider text-muted-foreground/70">
						Pending tasks
					</span>
					<ClipboardList class="size-4 text-amber-500/80" />
				</div>
				<div class="mt-2 flex items-baseline gap-1.5">
					<span class="text-3xl font-semibold tabular-nums text-foreground">{loading ? '—' : stats.pendingTasks}</span>
					<span class="text-sm text-muted-foreground">awaiting</span>
				</div>
			</a>

			<a
				href="/templates"
				data-testid="btn-view-templates"
				class="group rounded-xl border border-border bg-card p-4 transition-colors hover:bg-accent/50"
			>
				<div class="flex items-center justify-between">
					<span class="text-sm font-medium uppercase tracking-wider text-muted-foreground/70">
						Templates
					</span>
					<FileText class="size-4 text-violet-500/80" />
				</div>
				<div class="mt-2 flex items-baseline gap-1.5">
					<span class="text-3xl font-semibold tabular-nums text-foreground">{loading ? '—' : stats.templates}</span>
					<span class="text-sm text-muted-foreground">
						{stats.draftTemplates > 0 ? `${stats.draftTemplates} draft${stats.draftTemplates === 1 ? '' : 's'}` : 'all published'}
					</span>
				</div>
			</a>

			<a
				href="/instances?status=completed"
				class="group rounded-xl border border-border bg-card p-4 transition-colors hover:bg-accent/50"
			>
				<div class="flex items-center justify-between">
					<span class="text-sm font-medium uppercase tracking-wider text-muted-foreground/70">
						Today
					</span>
					<CheckCircle2 class="size-4 text-green-500/80" />
				</div>
				<div class="mt-2 flex items-baseline gap-1.5">
					<span class="text-3xl font-semibold tabular-nums text-foreground">{loading ? '—' : stats.completedToday}</span>
					<span class="text-sm text-muted-foreground">completed</span>
					{#if stats.failedToday > 0}
						<span class="ml-1 text-sm text-red-600">· {stats.failedToday} failed</span>
					{/if}
				</div>
			</a>
		</div>

		<!-- My inbox preview: tasks to claim + my open work (consent acceptance).
		     Only shown when there's something relevant, so it stays out of the
		     way for users with no human-task assignments. -->
		{#if hasInboxPreview}
			<div class="mt-8 grid grid-cols-1 gap-6 lg:grid-cols-2">
				<!-- Tasks to claim -->
				<section>
					<div class="mb-3 flex items-baseline justify-between">
						<h2 class="flex items-center gap-2 text-sm font-semibold uppercase tracking-wider text-muted-foreground">
							<Hand class="size-3.5" />
							Tasks to claim
							{#if offeredTasks.length > 0}
								<Badge variant="warm" class="rounded-full">{offeredTasks.length}</Badge>
							{/if}
						</h2>
						<a
							href="/tasks/inbox"
							class="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
						>
							Open inbox <ArrowRight class="size-3" />
						</a>
					</div>

					{#if offeredTasks.length === 0}
						<div
							class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-8"
						>
							<Inbox class="size-7 text-muted-foreground/40" />
							<p class="mt-2 text-sm text-muted-foreground">No open offers</p>
						</div>
					{:else}
						<div class="space-y-1.5">
							{#each offeredTasks.slice(0, 4) as task (task.task_id)}
								<a
									href="/tasks/{task.task_id}"
									class="group flex items-center justify-between rounded-lg border border-border bg-card px-3.5 py-2.5 transition-colors hover:bg-accent/50"
									data-testid="home-offered-{task.task_id}"
								>
									<div class="min-w-0">
										<span class="truncate text-sm font-medium text-foreground">{task.title}</span>
										<p class="mt-0.5 truncate text-sm text-muted-foreground">
											Offered {formatRelative(task.created_at)}
										</p>
									</div>
									<ArrowRight
										class="size-4 shrink-0 text-muted-foreground/40 transition-all group-hover:translate-x-0.5 group-hover:text-foreground"
									/>
								</a>
							{/each}
							{#if offeredTasks.length > 4}
								<a
									href="/tasks/inbox"
									class="block px-3.5 py-1.5 text-sm text-muted-foreground hover:text-foreground"
								>
									+{offeredTasks.length - 4} more
								</a>
							{/if}
						</div>
					{/if}
				</section>

				<!-- My open tasks -->
				<section>
					<div class="mb-3 flex items-baseline justify-between">
						<h2 class="flex items-center gap-2 text-sm font-semibold uppercase tracking-wider text-muted-foreground">
							<ClipboardList class="size-3.5" />
							My open tasks
							{#if myOpenTasks.length > 0}
								<Badge variant="warm" class="rounded-full">{myOpenTasks.length}</Badge>
							{/if}
						</h2>
						<a
							href="/tasks/inbox"
							class="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
						>
							View all <ArrowRight class="size-3" />
						</a>
					</div>

					{#if myOpenTasks.length === 0}
						<div
							class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-8"
						>
							<ClipboardList class="size-7 text-muted-foreground/40" />
							<p class="mt-2 text-sm text-muted-foreground">Nothing in progress</p>
						</div>
					{:else}
						<div class="space-y-1.5">
							{#each myOpenTasks.slice(0, 4) as task (task.task_id)}
								<a
									href="/tasks/{task.task_id}"
									class="group flex items-center justify-between rounded-lg border border-border bg-card px-3.5 py-2.5 transition-colors hover:bg-accent/50"
									data-testid="home-open-task-{task.task_id}"
								>
									<div class="min-w-0">
										<span class="truncate text-sm font-medium text-foreground">{task.title}</span>
										<p class="mt-0.5 truncate text-sm text-muted-foreground">
											Claimed {formatRelative(task.created_at)}
										</p>
									</div>
									<ArrowRight
										class="size-4 shrink-0 text-muted-foreground/40 transition-all group-hover:translate-x-0.5 group-hover:text-foreground"
									/>
								</a>
							{/each}
							{#if myOpenTasks.length > 4}
								<a
									href="/tasks/inbox"
									class="block px-3.5 py-1.5 text-sm text-muted-foreground hover:text-foreground"
								>
									+{myOpenTasks.length - 4} more
								</a>
							{/if}
						</div>
					{/if}
				</section>
			</div>
		{/if}

		{#if isEmpty}
			<!-- Empty state: a clean "let's get started" -->
			<div
				class="mt-10 flex flex-col items-center justify-center rounded-xl border border-dashed border-border bg-card/60 px-6 py-16 text-center"
			>
				<div class="flex size-12 items-center justify-center rounded-2xl bg-primary/10">
					<Rocket class="size-6 text-primary" />
				</div>
				<h2 class="mt-4 text-lg font-semibold text-foreground">Get started</h2>
				<p class="mt-1 max-w-sm text-sm text-muted-foreground">
					Open the seeded demo to explore a complete workflow, or design your own from a blank canvas.
				</p>
				<div class="mt-5 flex items-center gap-2">
					<Button onclick={openDemo} disabled={openingDemo}>
						<Rocket class="size-4" />
						{openingDemo ? 'Opening…' : 'Open Demo'}
					</Button>
					<Button variant="outline" onclick={newTemplate} disabled={creatingTemplate}>
						<Plus class="size-4" />
						New Template
					</Button>
				</div>
			</div>
		{:else}
			<!-- Two-column: active instances · recent templates ----------- -->
			<div class="mt-8 grid grid-cols-1 gap-6 lg:grid-cols-2">
				<!-- Active instances -->
				<section>
					<div class="mb-3 flex items-baseline justify-between">
						<h2 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground">
							{runningInstances.length > 0 ? 'Active instances' : 'Recent instances'}
						</h2>
						<a
							href="/instances"
							class="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
						>
							View all <ArrowRight class="size-3" />
						</a>
					</div>

					{#if loading}
						<div class="rounded-xl border border-border bg-card p-6 text-sm text-muted-foreground">
							Loading…
						</div>
					{:else if recentInstances.length === 0}
						<div
							class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-10"
						>
							<Activity class="size-8 text-muted-foreground/40" />
							<p class="mt-2 text-sm text-muted-foreground">No instances yet</p>
						</div>
					{:else}
						<div class="space-y-1.5">
							{#each recentInstances as instance (instance.id)}
								<a
									href="/instances/{instance.id}"
									class="group flex items-center justify-between rounded-lg border border-border bg-card px-3.5 py-2.5 transition-colors hover:bg-accent/50"
									data-testid="home-instance-{instance.id}"
								>
									<div class="min-w-0">
										<div class="flex items-center gap-2">
											<span class="truncate text-sm font-medium text-foreground">
												{instance.template_name ?? instance.net_id}
											</span>
											<StatusBadge domain="workflow" status={instance.status} />
										</div>
										<p class="mt-0.5 truncate text-sm text-muted-foreground">
											{#if instance.current_step}
												<span>{instance.current_step}</span>
												<span class="mx-1">·</span>
											{/if}
											<span>{formatRelative(instance.created_at)}</span>
										</p>
									</div>
									<ArrowRight
										class="size-4 shrink-0 text-muted-foreground/40 transition-all group-hover:translate-x-0.5 group-hover:text-foreground"
									/>
								</a>
							{/each}
						</div>
					{/if}
				</section>

				<!-- Recent templates -->
				<section>
					<div class="mb-3 flex items-baseline justify-between">
						<h2 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground">
							Recent templates
						</h2>
						<a
							href="/templates"
							class="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
						>
							View all <ArrowRight class="size-3" />
						</a>
					</div>

					{#if loading}
						<div class="rounded-xl border border-border bg-card p-6 text-sm text-muted-foreground">
							Loading…
						</div>
					{:else if recentTemplates.length === 0}
						<div
							class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-10"
						>
							<FileText class="size-8 text-muted-foreground/40" />
							<p class="mt-2 text-sm text-muted-foreground">No templates yet</p>
							<Button class="mt-3" size="sm" onclick={newTemplate} disabled={creatingTemplate}>
								<Plus class="size-4" />
								New Template
							</Button>
						</div>
					{:else}
						<div class="space-y-1.5">
							{#each recentTemplates as template (template.id)}
								<a
									href="/templates/{template.id}"
									class="group flex items-center justify-between rounded-lg border border-border bg-card px-3.5 py-2.5 transition-colors hover:bg-accent/50"
									data-testid="home-template-{template.id}"
								>
									<div class="min-w-0">
										<div class="flex items-center gap-2">
											<span class="truncate text-sm font-medium text-foreground">
												{template.name}
											</span>
											<Badge
												variant="secondary"
												class={template.published
													? 'bg-green-100 text-green-700'
													: 'bg-amber-100 text-amber-700'}
											>
												{template.published ? `v${template.version}` : `Draft v${template.version}`}
											</Badge>
										</div>
										<p class="mt-0.5 truncate text-sm text-muted-foreground">
											{template.description?.trim() || 'No description'}
											<span class="mx-1">·</span>
											<span>Updated {formatRelative(template.updated_at)}</span>
										</p>
									</div>
									<ArrowRight
										class="size-4 shrink-0 text-muted-foreground/40 transition-all group-hover:translate-x-0.5 group-hover:text-foreground"
									/>
								</a>
							{/each}
						</div>
					{/if}
				</section>
			</div>
		{/if}
		</div>
	</div>
</div>
