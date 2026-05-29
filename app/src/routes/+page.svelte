<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import {
		listInstances,
		listTemplates,
		listTasks,
		createTemplate,
		type InstanceListItem,
		type TemplateSummary
	} from '$lib/api/client';
	import { findShowcaseTemplate } from '$lib/templates/showcase';
	import { auth } from '$lib/auth/store.svelte';
	import Rocket from '@lucide/svelte/icons/rocket';
	import Plus from '@lucide/svelte/icons/plus';
	import Activity from '@lucide/svelte/icons/activity';
	import ClipboardList from '@lucide/svelte/icons/clipboard-list';
	import FileText from '@lucide/svelte/icons/file-text';
	import CheckCircle2 from '@lucide/svelte/icons/check-circle-2';
	import ArrowRight from '@lucide/svelte/icons/arrow-right';

	let openingDemo = $state(false);
	let demoError = $state<string | null>(null);

	let loading = $state(true);
	let loadError = $state<string | null>(null);
	let runningInstances = $state<InstanceListItem[]>([]);
	let recentInstances = $state<InstanceListItem[]>([]);
	let recentTemplates = $state<TemplateSummary[]>([]);
	let stats = $state({
		running: 0,
		pendingTasks: 0,
		templates: 0,
		draftTemplates: 0,
		completedToday: 0,
		failedToday: 0
	});

	const statusStyles: Record<string, string> = {
		running: 'bg-blue-100 text-blue-700',
		created: 'bg-gray-100 text-gray-700',
		completed: 'bg-green-100 text-green-700',
		failed: 'bg-red-100 text-red-700',
		cancelled: 'bg-slate-100 text-slate-700'
	};

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

			const [running, completedRecent, failedRecent, templates, pendingTasks] = await Promise.all([
				listInstances({ status: 'running', perPage: 6 }),
				listInstances({ status: 'completed', perPage: 20 }),
				listInstances({ status: 'failed', perPage: 20 }),
				listTemplates({ pageSize: 50 }),
				listTasks({ status: 'pending', limit: 1 })
			]);

			runningInstances = running.items;
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

<div class="h-full overflow-y-auto" data-testid="home-page">
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
					variant="outline"
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
				class="group rounded-xl border border-border bg-card p-4 transition-colors hover:bg-accent/50"
			>
				<div class="flex items-center justify-between">
					<span class="text-sm font-medium uppercase tracking-wider text-muted-foreground/70">
						Running
					</span>
					<Activity class="size-4 text-blue-500/70" />
				</div>
				<div class="mt-2 flex items-baseline gap-1.5">
					<span class="text-3xl font-semibold tabular-nums text-foreground">{loading ? '—' : stats.running}</span>
					<span class="text-sm text-muted-foreground">
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
											<Badge
												variant="secondary"
												class={statusStyles[instance.status] ?? ''}
											>
												{instance.status}
											</Badge>
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
