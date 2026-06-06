<script lang="ts">
	import { page } from '$app/state';
	import {
		getInstance,
		cancelInstance,
		listProcessesByInstance,
		instanceStreamUrl
	} from '$lib/api/client';
	import { authFetch } from '$lib/auth/fetch';
	import { connectSse, type SseConnection } from '$lib/net/sse';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import {
		provideInstanceContext,
		type InstanceContext
	} from '$lib/components/instances/instance-context';
	import SaveAsTestDialog from '$lib/components/instances/SaveAsTestDialog.svelte';
	import FileText from '@lucide/svelte/icons/file-text';
	import LayoutDashboard from '@lucide/svelte/icons/layout-dashboard';
	import ListChecks from '@lucide/svelte/icons/list-checks';
	import Workflow from '@lucide/svelte/icons/workflow';
	import Network from '@lucide/svelte/icons/network';
	import FlaskConical from '@lucide/svelte/icons/flask-conical';
	import CornerLeftUp from '@lucide/svelte/icons/corner-left-up';

	let saveAsTestOpen = $state(false);

	let { children } = $props();

	const instanceId = $derived(page.params.id!);
	const pathname = $derived(page.url.pathname);

	// Single reactive store shared with every subroute. Subpages mutate
	// `instance`/`processes`/etc. through `reload()`; we never re-assign the
	// object itself so the context handle stays stable.
	// svelte-ignore state_referenced_locally
	const ctx = $state<InstanceContext>({
		instanceId,
		instance: null,
		processes: [],
		loading: true,
		error: null,
		reload
	});

	provideInstanceContext(ctx);

	const statusColors: Record<string, string> = {
		created: 'bg-gray-100 text-gray-700',
		running: 'bg-blue-100 text-blue-700',
		completed: 'bg-green-100 text-green-700',
		failed: 'bg-red-100 text-red-700',
		cancelled: 'bg-slate-100 text-slate-700'
	};

	const formatDate = (s: string | null) => (s ? new Date(s).toLocaleString() : '-');

	const hasNet = $derived(
		!!ctx.instance && ctx.instance.status !== 'created' && !!ctx.instance.net_id
	);
	const primaryProcess = $derived(ctx.processes[0] ?? null);
	const processName = $derived(primaryProcess?.name ?? null);

	// `silent` refetches (driven by the live SSE stream below) update
	// instance/processes in place without toggling `ctx.loading`, so the live
	// status updates never flash the page-level loading spinner.
	async function reload({ silent = false }: { silent?: boolean } = {}) {
		if (!silent) ctx.loading = true;
		ctx.error = null;
		try {
			ctx.instance = await getInstance(ctx.instanceId);
			try {
				ctx.processes = (await listProcessesByInstance(ctx.instanceId)).items;
			} catch {
				ctx.processes = [];
			}
		} catch (e) {
			ctx.error = e instanceof Error ? e.message : 'Failed to load instance';
		} finally {
			if (!silent) ctx.loading = false;
		}
	}

	// ── Live instance state ─────────────────────────────────────────────────
	// The per-tab panels stream/poll their own data, but the always-visible
	// header summary (status badge, timestamps, current_step) and the process
	// header live on `ctx`, which used to load only once. Subscribe to the
	// instance's domain-event SSE stream and treat any event as a debounced
	// "something changed → refetch ctx" trigger. The stream replays from the
	// start (no resume cursor), so the debounce coalesces that burst — and any
	// live burst — into a single refetch, keeping us decoupled from the
	// domain-event taxonomy. Mirrors the pattern in stores/tasks.svelte.ts.
	let sseConnection: SseConnection | null = null;
	let refetchTimer: ReturnType<typeof setTimeout> | null = null;

	function scheduleRefetch() {
		if (refetchTimer !== null) return;
		refetchTimer = setTimeout(() => {
			refetchTimer = null;
			reload({ silent: true });
		}, 250);
	}

	let terminalPollTimer: ReturnType<typeof setTimeout> | null = null;

	function closeStream() {
		sseConnection?.close();
		sseConnection = null;
		if (refetchTimer !== null) {
			clearTimeout(refetchTimer);
			refetchTimer = null;
		}
		if (terminalPollTimer !== null) {
			clearTimeout(terminalPollTimer);
			terminalPollTimer = null;
		}
	}

	const TERMINAL_STATUSES = new Set(['completed', 'failed', 'cancelled']);

	// The server derives the `result` SSE event straight from the engine's
	// terminal domain event (NetCompleted/NetCancelled), but the
	// `workflow_instances` row that `getInstance()` reads is written by a
	// SEPARATE async lifecycle consumer. So the `result` event can arrive
	// before the row flips to a terminal status — a single reload here would
	// read stale `running` data and, since we then close the stream, never
	// self-correct. Poll the projection until it catches up (bounded), then
	// close. Each step's panels stop their own polling on terminal status, so
	// landing it here is what makes the whole page settle.
	function reloadUntilTerminal(attemptsLeft: number) {
		if (terminalPollTimer !== null) {
			clearTimeout(terminalPollTimer);
			terminalPollTimer = null;
		}
		reload({ silent: true }).then(() => {
			const status = ctx.instance?.status;
			if (attemptsLeft <= 0 || (status && TERMINAL_STATUSES.has(status))) {
				closeStream();
			} else {
				terminalPollTimer = setTimeout(
					() => reloadUntilTerminal(attemptsLeft - 1),
					300
				);
			}
		});
	}

	function openStream(id: string) {
		closeStream();
		sseConnection = connectSse(instanceStreamUrl(id), {
			fetchImpl: authFetch,
			maxRetries: 5,
			initialRetryMs: 1000,
			// 404/4xx (e.g. instance not found): retrying can never succeed.
			onTerminal: () => closeStream(),
			onEvent: ({ event }) => {
				if (event === 'result') {
					// Terminal: refetch until the row lands a terminal status
					// (closing the lifecycle-consumer race), then close.
					reloadUntilTerminal(10);
				} else if (event !== 'connected') {
					scheduleRefetch();
				}
			}
		});
	}

	async function handleCancel() {
		if (!ctx.instance || !confirm('Cancel this instance?')) return;
		try {
			await cancelInstance(ctx.instance.id);
			ctx.instance = { ...ctx.instance, status: 'cancelled' };
		} catch (e) {
			ctx.error = e instanceof Error ? e.message : 'Failed to cancel';
		}
	}

	$effect(() => {
		ctx.instanceId = instanceId;
		reload();
		openStream(instanceId);
		// Re-run on instanceId change tears down the old stream and opens the
		// new one; unmount closes it.
		return () => closeStream();
	});

	type TabDef = {
		href: string;
		match: string;
		label: string;
		icon: typeof LayoutDashboard;
		tone?: 'muted';
		title?: string;
	};

	const tabs = $derived<TabDef[]>([
		{
			href: `/instances/${instanceId}/process`,
			match: 'process',
			label: 'Process',
			icon: LayoutDashboard
		},
		...(hasNet
			? [
					{
						href: `/instances/${instanceId}/workflow`,
						match: 'workflow',
						label: 'Workflow',
						icon: Workflow,
						title: 'Template graph overlaid with per-step runtime status'
					},
					{
						href: `/instances/${instanceId}/steps`,
						match: 'steps',
						label: 'Steps',
						icon: ListChecks,
						title: 'Per-step runtime as a table — every iteration as a row'
					},
					{
						href: `/instances/${instanceId}/petri-net`,
						match: 'petri-net',
						label: 'Petri net',
						icon: Network,
						tone: 'muted' as const,
						title: 'Engine debug: the raw Petri net for this run'
					}
				]
			: [])
	]);

	function isActive(match: string): boolean {
		return pathname.startsWith(`/instances/${instanceId}/${match}`);
	}
</script>

<div class="flex h-full flex-col" data-testid="instance-page">
	{#if ctx.loading && !ctx.instance}
		<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
			Loading...
		</div>
	{:else if ctx.error && !ctx.instance}
		<div
			class="mx-6 mt-6 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800"
		>
			{ctx.error}
		</div>
	{:else if ctx.instance}
		<div class="border-b border-border bg-card px-4 py-2 shrink-0">
			{#if ctx.instance.parent_instance_id}
				<!-- This run was spawned by a SubWorkflow node in a parent run.
				     A plain <a> is correct: navigating to the parent is a fresh
				     /instances/[id] mount (new InstanceContext). Each ancestor
				     page shows its own parent link, so the chain is climbable. -->
				<a
					href={`/instances/${ctx.instance.parent_instance_id}/workflow`}
					class="mb-1 inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
					data-testid="parent-instance-breadcrumb"
				>
					<CornerLeftUp class="size-3.5" />
					Parent run
				</a>
			{/if}
			<div class="flex items-center justify-between gap-3">
				<div class="flex items-center gap-3 min-w-0">
					<h1 class="shrink-0 text-base font-semibold text-foreground">
						{processName ?? 'Run'}
					</h1>
					<Badge class={statusColors[ctx.instance.status] ?? ''} variant="secondary">
						{ctx.instance.status}
					</Badge>
					<span class="font-mono text-sm text-muted-foreground truncate">
						{ctx.instance.net_id}
					</span>
				</div>
				<div class="flex items-center gap-2 shrink-0">
					<Button variant="ghost" size="sm" href="/templates/{ctx.instance.template_id}">
						<FileText class="size-3.5" />
						Template v{ctx.instance.template_version}
					</Button>
					{#if ctx.instance.status === 'running' || ctx.instance.status === 'created'}
						<Button
							variant="outline"
							size="sm"
							class="border-destructive/30 text-destructive hover:bg-destructive/10"
							onclick={handleCancel}
						>
							Cancel
						</Button>
					{:else if ctx.instance.mode !== 'test_run'}
						<Button
							variant="outline"
							size="sm"
							onclick={() => (saveAsTestOpen = true)}
							data-testid="save-as-test"
						>
							<FlaskConical class="mr-1 size-3.5" />
							Save as test
						</Button>
					{/if}
				</div>
			</div>
			<div class="mt-1 flex flex-wrap gap-x-4 gap-y-0.5 text-sm text-muted-foreground">
				<span>created {formatDate(ctx.instance.created_at)}</span>
				<span>started {formatDate(ctx.instance.started_at ?? null)}</span>
				<span>completed {formatDate(ctx.instance.completed_at ?? null)}</span>
				{#if ctx.instance.current_step}
					<span class="text-foreground">step: {ctx.instance.current_step}</span>
				{/if}
			</div>
		</div>

		{#if primaryProcess || hasNet}
			<!-- Tab subroutes: Process (HPI) is primary; Workflow shows the
			     template graph overlaid with per-step runtime info; Steps is the
			     same data as a table; Petri net is the engine debug view. Each
			     is a proper subpage — navigation unmounts the previous view. -->
			<nav
				class="flex items-center gap-1 border-b border-border bg-card px-3 py-1 shrink-0"
				data-testid="instance-tabs"
			>
				{#each tabs as tab (tab.match)}
					{@const active = isActive(tab.match)}
					{@const Icon = tab.icon}
					<a
						href={tab.href}
						class="inline-flex items-center gap-1.5 rounded-md px-2.5 py-1 text-sm font-medium transition-colors
							{active
							? tab.tone === 'muted'
								? 'bg-accent text-foreground'
								: 'bg-primary text-primary-foreground'
							: tab.tone === 'muted'
								? 'text-muted-foreground/70 hover:bg-accent hover:text-foreground'
								: 'text-muted-foreground hover:bg-accent hover:text-foreground'}"
						title={tab.title}
						data-testid="instance-tab-{tab.match}"
						aria-current={active ? 'page' : undefined}
					>
						<Icon class="size-3.5" />
						{tab.label}
					</a>
				{/each}
			</nav>

			<div class="relative flex-1 min-h-0">
				{@render children()}
			</div>
		{:else}
			<div
				class="flex flex-1 items-center justify-center py-16 text-sm text-muted-foreground"
			>
				Instance has not started yet. No Petri net is available.
			</div>
		{/if}
	{/if}
</div>

{#if ctx.instance}
	<SaveAsTestDialog
		open={saveAsTestOpen}
		instanceId={ctx.instance.id}
		templateId={ctx.instance.template_id}
		onclose={() => (saveAsTestOpen = false)}
	/>
{/if}
