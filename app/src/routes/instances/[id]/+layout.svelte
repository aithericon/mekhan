<script lang="ts">
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import {
		getInstance,
		cancelInstance,
		listProcessesByInstance,
		listStepExecutions,
		instanceStreamUrl
	} from '$lib/api/client';
	import { authFetch } from '$lib/auth/fetch';
	import { connectSse, type SseConnection } from '$lib/net/sse';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import {
		PageShell,
		PageHeader,
		PageTabs,
		type PageTab
	} from '$lib/components/shell';
	import {
		provideInstanceContext,
		type InstanceContext
	} from '$lib/components/instances/instance-context';
	import SaveAsTestDialog from '$lib/components/instances/SaveAsTestDialog.svelte';
	import CreateInstanceDialog from '$lib/components/instances/CreateInstanceDialog.svelte';
	import ShareDialog from '$lib/components/iam/ShareDialog.svelte';
	import AuthorshipChips from '$lib/components/iam/AuthorshipChips.svelte';
	import { roleAtLeast } from '$lib/api/iam';
	import FileText from '@lucide/svelte/icons/file-text';
	import Share2 from '@lucide/svelte/icons/share-2';
	import LayoutDashboard from '@lucide/svelte/icons/layout-dashboard';
	import ListChecks from '@lucide/svelte/icons/list-checks';
	import Workflow from '@lucide/svelte/icons/workflow';
	import Network from '@lucide/svelte/icons/network';
	import FlaskConical from '@lucide/svelte/icons/flask-conical';
	import RotateCcw from '@lucide/svelte/icons/rotate-ccw';
	import CornerLeftUp from '@lucide/svelte/icons/corner-left-up';

	let saveAsTestOpen = $state(false);
	let shareOpen = $state(false);

	// ── Rerun ────────────────────────────────────────────────────────────────
	// Re-launch this instance's template, pre-filling the launch sheet with the
	// start parameters this run was created with (read from the Start node's
	// recorded output token). The user can tweak any field before launching.
	let rerunOpen = $state(false);
	let rerunLoading = $state(false);
	let rerunInitial = $state<Record<string, Record<string, unknown>> | null>(null);

	async function handleRerun() {
		if (!ctx.instance) return;
		rerunLoading = true;
		try {
			// The Start node's step execution emits a token = the start parameters,
			// keyed by field name (plus `_`-prefixed metadata the dialog ignores).
			const execs = await listStepExecutions(ctx.instance.id);
			const seed: Record<string, Record<string, unknown>> = {};
			for (const e of execs) {
				if (e.node_kind === 'start' && e.outputs && typeof e.outputs === 'object') {
					seed[e.node_id] = e.outputs as Record<string, unknown>;
				}
			}
			rerunInitial = seed;
			rerunOpen = true;
		} catch (e) {
			ctx.error = e instanceof Error ? e.message : 'Failed to load start parameters';
		} finally {
			rerunLoading = false;
		}
	}

	let { children } = $props();

	const instanceId = $derived(page.params.id!);

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
	// Structural equality via JSON. The instance / process DTOs are plain JSON
	// with stable key order across calls, so this is a cheap, sufficient change
	// detector — and it lets us keep ctx object identity stable across no-op
	// refetches (see reload()).
	function jsonEqual(a: unknown, b: unknown): boolean {
		return JSON.stringify(a) === JSON.stringify(b);
	}

	async function reload({ silent = false }: { silent?: boolean } = {}) {
		if (!silent) ctx.loading = true;
		ctx.error = null;
		try {
			// Only REASSIGN when the payload actually changed. reload() fires on a
			// debounced SSE trigger throughout a run and getInstance() returns a
			// fresh object every call, so an unconditional assignment would flip
			// ctx.instance's identity on every tick even when nothing the UI shows
			// moved. Every tab takes `instance` as a prop, so that identity churn
			// re-runs child $effects — closing the workflow drawer and tearing the
			// live Channels player down before it can paint. A no-op refetch must
			// be a no-op for reactivity.
			const nextInstance = await getInstance(ctx.instanceId);
			if (!ctx.instance || !jsonEqual(ctx.instance, nextInstance)) {
				ctx.instance = nextInstance;
			}
			try {
				const nextProcesses = (await listProcessesByInstance(ctx.instanceId)).items;
				if (!jsonEqual(ctx.processes, nextProcesses)) ctx.processes = nextProcesses;
			} catch {
				if (ctx.processes.length) ctx.processes = [];
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

	// Per-token / per-effect value events fire hundreds of times a second during
	// a run (a streaming workflow emits one TokenCreated + EffectCompleted per
	// frame). They never change the header summary the layout owns
	// (status / current_step / process), so they must NOT drive a header
	// refetch — otherwise the running phase floods the API (~20 req/s) and
	// churns every tab. Structural events (TransitionFired, NetInitialized, …)
	// still trigger one coalesced refetch; `result` is handled separately.
	const HEADER_NOISE_EVENTS = new Set(['TokenCreated', 'EffectCompleted']);

	function scheduleRefetch() {
		if (refetchTimer !== null) return;
		refetchTimer = setTimeout(() => {
			refetchTimer = null;
			reload({ silent: true });
		}, 1000);
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
				} else if (event !== 'connected' && !HEADER_NOISE_EVENTS.has(event)) {
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

	// Tab subroutes: Process (HPI) is primary; Workflow shows the template
	// graph overlaid with per-step runtime info; Steps is the same data as a
	// table; Petri net is the engine debug view. Each is a proper subpage —
	// navigation unmounts the previous view.
	const tabs = $derived<PageTab[]>([
		{
			href: `/instances/${instanceId}/process`,
			label: 'Process',
			icon: LayoutDashboard,
			testid: 'instance-tab-process'
		},
		...(hasNet
			? [
					{
						href: `/instances/${instanceId}/workflow`,
						label: 'Workflow',
						icon: Workflow,
						title: 'Template graph overlaid with per-step runtime status',
						testid: 'instance-tab-workflow'
					},
					{
						href: `/instances/${instanceId}/steps`,
						label: 'Steps',
						icon: ListChecks,
						title: 'Per-step runtime as a table — every iteration as a row',
						testid: 'instance-tab-steps'
					},
					{
						href: `/instances/${instanceId}/petri-net`,
						label: 'Petri net',
						icon: Network,
						title: 'Engine debug: the raw Petri net for this run',
						testid: 'instance-tab-petri-net'
					}
				]
			: [])
	]);
</script>

<!-- Full-bleed shell: the subpages (workflow / petri-net canvases, steps /
     process scrollers) position `absolute inset-0` against the
     `relative flex-1 min-h-0` wrapper below and own their own scroll, so the
     band variant's padded scroll content area would break them. -->
<PageShell width="bleed" testid="instance-page">
	<div class="flex h-full flex-col">
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
			{@const instance = ctx.instance}
			<!-- Hand-rolled band (bleed shell): same tokens/anatomy as PageShell's
			     band variant — header row + flush tab row over one border-b, the
			     tab underline overlapping it via -mb-px. The body below is
			     full-width, so the band content anchors flush LEFT (not the
			     centered 6xl grid) — header and body share the same left edge. -->
			<div class="shrink-0 border-b border-border bg-card px-6 pt-4">
				<div class="w-full">
					{#if instance.parent_instance_id}
						<!-- This run was spawned by a SubWorkflow node in a parent run.
						     A plain <a> is correct: navigating to the parent is a fresh
						     /instances/[id] mount (new InstanceContext). Each ancestor
						     page shows its own parent link, so the chain is climbable. -->
						<a
							href={`/instances/${instance.parent_instance_id}/workflow`}
							class="mb-1 inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
							data-testid="parent-instance-breadcrumb"
						>
							<CornerLeftUp class="size-3.5" />
							Parent run
						</a>
					{/if}
					<PageHeader title={processName ?? 'Run'} variant="detail" class="mb-0">
						<div
							class="mt-1 flex flex-wrap items-center gap-x-3 gap-y-0.5 text-sm text-muted-foreground"
						>
							<Badge class={statusColors[instance.status] ?? ''} variant="secondary">
								{instance.status}
							</Badge>
							<span class="font-mono truncate">{instance.net_id}</span>
						</div>
						<div class="mt-1 flex flex-wrap gap-x-4 gap-y-0.5 text-sm text-muted-foreground">
							<span>started {formatDate(instance.started_at ?? null)}</span>
							<span>completed {formatDate(instance.completed_at ?? null)}</span>
							{#if instance.current_step}
								<span class="text-foreground">step: {instance.current_step}</span>
							{/if}
						</div>
						<AuthorshipChips
							class="mt-1"
							createdBy={instance.created_by}
							createdAt={instance.created_at}
							updatedBy={instance.updated_by}
							updatedAt={instance.updated_at}
						/>
						{#snippet actions()}
							<Button variant="ghost" size="sm" href="/templates/{instance.template_id}">
								<FileText class="size-3.5" />
								Template v{instance.template_version}
							</Button>
							{#if roleAtLeast(instance.my_effective_role, 'admin')}
								<Button
									variant="outline"
									size="sm"
									onclick={() => (shareOpen = true)}
									data-testid="btn-share-instance"
								>
									<Share2 class="mr-1 size-3.5" />
									Share
								</Button>
							{/if}
							{#if instance.mode !== 'test_run'}
								<Button
									variant="outline"
									size="sm"
									onclick={handleRerun}
									disabled={rerunLoading}
									data-testid="rerun-instance"
									title="Launch this template again, pre-filled with this run's start parameters"
								>
									<RotateCcw class="mr-1 size-3.5" />
									{rerunLoading ? 'Loading…' : 'Rerun'}
								</Button>
							{/if}
							{#if (instance.status === 'running' || instance.status === 'created') && roleAtLeast(instance.my_effective_role, 'editor')}
								<Button
									variant="outline"
									size="sm"
									class="border-destructive/30 text-destructive hover:bg-destructive/10"
									onclick={handleCancel}
								>
									Cancel
								</Button>
							{:else if instance.status !== 'running' && instance.status !== 'created' && instance.mode !== 'test_run'}
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
						{/snippet}
					</PageHeader>

					{#if primaryProcess || hasNet}
						<div class="-mb-px mt-1">
							<PageTabs testid="instance-tabs" {tabs} />
						</div>
					{:else}
						<div class="pb-3"></div>
					{/if}
				</div>
			</div>

			{#if primaryProcess || hasNet}
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
</PageShell>

{#if ctx.instance}
	<SaveAsTestDialog
		open={saveAsTestOpen}
		instanceId={ctx.instance.id}
		templateId={ctx.instance.template_id}
		onclose={() => (saveAsTestOpen = false)}
	/>
	<CreateInstanceDialog
		bind:open={rerunOpen}
		templateId={ctx.instance.template_id}
		initialValues={rerunInitial}
		title="Rerun instance"
		description="Pre-filled with this run's start parameters — adjust and launch again."
		oncreated={(id) => {
			rerunOpen = false;
			void goto(`/instances/${id}`);
		}}
	/>
	<ShareDialog
		bind:open={shareOpen}
		objectType="instance"
		objectId={ctx.instance.id}
		objectName={processName ?? 'this run'}
		myEffectiveRole={ctx.instance.my_effective_role}
		onChanged={() => reload({ silent: true })}
	/>
{/if}
