<script lang="ts">
	import { page } from '$app/stores';
	import { getTask, completeTask, cancelTask, claimTask, getProcess } from '$lib/api/client';
	import type { HumanTask, ProcessState } from '$lib/types/tasks';
	import { HumanTaskPanel, TaskForm, SuccessOverlay } from '$lib/components/tasks';
	import { PageShell, PageHeader } from '$lib/components/shell';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import ArrowRight from '@lucide/svelte/icons/arrow-right';

	let task: HumanTask | null = $state(null);
	let process: ProcessState | null = $state(null);
	// Note: getProcess now returns ProcessDetail — cast for backward compat
	let loading = $state(true);
	let error: string | null = $state(null);
	let submitting = $state(false);
	let claiming = $state(false);
	let showSuccess = $state(false);

	const taskId = $derived($page.params.id as string);

	// Where to send the user when they leave this task. ProcessView links here
	// with ?from=<instance/process page> so "back" returns to the run they were
	// working in rather than the global task list. Only same-origin absolute
	// paths are honoured (guards against open-redirect via a crafted ?from).
	const backHref = $derived.by(() => {
		const from = $page.url.searchParams.get('from');
		if (!from || !from.startsWith('/') || from.startsWith('//') || from.startsWith('/\\')) {
			return '/tasks';
		}
		return from;
	});
	const backLabel = $derived(
		backHref.startsWith('/instances/')
			? 'Back to run'
			: backHref.startsWith('/processes/')
				? 'Back to process'
				: 'Back to tasks'
	);

	// Status badge under the title for the actionable states (mirrors the
	// branches that render a working card below); terminal states render their
	// status inside HumanTaskPanel instead.
	const statusBadge = $derived.by(() => {
		if (!task) return null;
		if (task.status === 'offered') return 'Offered';
		if ((task.status === 'pending' || task.status === 'claimed') && task.steps?.length) {
			return task.status === 'claimed' ? 'Claimed' : 'Pending';
		}
		return null;
	});

	async function load() {
		loading = true;
		error = null;
		try {
			task = await getTask(taskId);
			if (task.process_id) {
				try {
					process = await getProcess(task.process_id) as unknown as ProcessState;
				} catch {
					process = null;
				}
			}
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load task';
		} finally {
			loading = false;
		}
	}

	async function handleComplete(data: Record<string, unknown>) {
		if (!task) return;
		submitting = true;
		try {
			await completeTask(task.task_id, data);
			showSuccess = true;
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to complete task';
		} finally {
			submitting = false;
		}
	}

	async function handleClaim() {
		if (!task) return;
		claiming = true;
		error = null;
		try {
			await claimTask(task.task_id);
			// 202 — the authoritative `claimed` flip arrives via the pool-net
			// projection. Poll a few times until the row leaves `offered`.
			for (let i = 0; i < 8; i++) {
				await new Promise((r) => setTimeout(r, 600));
				await load();
				if (task && task.status !== 'offered') break;
			}
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to claim task';
		} finally {
			claiming = false;
		}
	}

	async function handleCancel() {
		if (!task) return;
		const reason = prompt('Reason for cancellation (optional):');
		if (reason === null) return;
		submitting = true;
		try {
			await cancelTask(task.task_id, reason || undefined);
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to cancel task';
		} finally {
			submitting = false;
		}
	}

	$effect(() => {
		taskId;
		load();
	});
</script>

<PageShell testid="task-detail-page">
	<PageHeader
		title={task?.title ?? 'Task'}
		variant="detail"
		back={{ href: backHref, label: backLabel }}
	>
		{#snippet children()}
			{#if statusBadge}
				<div class="mt-1">
					<Badge class="bg-amber-100 text-amber-700" variant="secondary">{statusBadge}</Badge>
				</div>
			{/if}
		{/snippet}
		{#snippet actions()}
			{#if task?.process_id && process}
				<Button
					variant="ghost"
					size="sm"
					href="/processes/{process.process_id}"
					class="text-muted-foreground"
				>
					View Process
				</Button>
			{/if}
		{/snippet}
	</PageHeader>

	{#if error}
		<div class="mb-4 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
			{error}
		</div>
	{/if}

	{#if loading}
		<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
			Loading...
		</div>
	{:else if task}
		{#if task.status === 'offered'}
			<!-- Offered to the capacity pool: the member claims to bind it. -->
			<div class="rounded-xl border border-amber-200 bg-card">
				{#if task.instructions_mdsvex}
					<div class="border-b border-border p-4">
						<p class="text-sm text-muted-foreground">{task.instructions_mdsvex}</p>
					</div>
				{/if}
				<div class="flex items-center justify-between gap-3 p-4">
					<p class="text-sm text-muted-foreground">
						This task is offered to everyone available in its capacity. Claim it to take it on —
						first claim wins.
					</p>
					<Button onclick={handleClaim} disabled={claiming} data-testid="claim-button">
						{claiming ? 'Claiming…' : 'Claim'}
					</Button>
				</div>
			</div>
		{:else if (task.status === 'pending' || task.status === 'claimed') && task.steps?.length}
			<!-- Pending/claimed with steps: show interactive form -->
			<div class="rounded-xl border border-border bg-card">
				{#if task.instructions_mdsvex}
					<div class="border-b border-border p-4">
						<p class="text-sm text-muted-foreground">{task.instructions_mdsvex}</p>
					</div>
				{/if}

				<div class="p-4">
					<TaskForm
						steps={task.steps}
						taskId={task.task_id}
						taskData={task.payload}
						onsubmit={handleComplete}
						oncancel={handleCancel}
						{submitting}
					/>
				</div>
			</div>
		{:else}
			<!-- Terminal state or pending without steps (API doesn't return them) -->
			<div class="rounded-xl border border-border bg-card">
				<HumanTaskPanel {task} {process} />
			</div>

			<!-- Primary forward CTA for terminal tasks. The page is read-only
			     from here on — the next reasonable action is to return to
			     wherever the user came from (instance overview, process
			     timeline, or the global task list). `backHref` already
			     encodes that choice via the `?from=` param. -->
			<div class="mt-6 flex justify-end" data-print="hide">
				<Button href={backHref} class="gap-1.5">
					{backLabel.startsWith('Back to') ? backLabel.replace('Back to ', 'Continue to ') : 'Continue'}
					<ArrowRight class="size-4" />
				</Button>
			</div>
		{/if}
	{:else}
		<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
			Task not found
		</div>
	{/if}
</PageShell>

<SuccessOverlay visible={showSuccess} onDismiss={() => (showSuccess = false)} />
