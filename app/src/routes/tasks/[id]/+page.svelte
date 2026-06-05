<script lang="ts">
	import { page } from '$app/stores';
	import { getTask, completeTask, cancelTask, getProcess } from '$lib/api/client';
	import type { HumanTask, ProcessState } from '$lib/types/tasks';
	import { HumanTaskPanel, TaskForm, SuccessOverlay } from '$lib/components/tasks';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import ArrowLeft from '@lucide/svelte/icons/arrow-left';
	import ArrowRight from '@lucide/svelte/icons/arrow-right';

	let task: HumanTask | null = $state(null);
	let process: ProcessState | null = $state(null);
	// Note: getProcess now returns ProcessDetail — cast for backward compat
	let loading = $state(true);
	let error: string | null = $state(null);
	let submitting = $state(false);
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

<div class="h-full overflow-y-auto">
	<div class="mx-auto max-w-5xl px-6 py-8 animate-rise">
		<div class="mb-4 flex items-center gap-2">
			<Button variant="ghost" size="sm" href={backHref} class="gap-1 text-muted-foreground">
				<ArrowLeft class="size-4" />
				{backLabel}
			</Button>
			{#if task?.process_id && process}
				<Button variant="ghost" size="sm" href="/processes/{process.process_id}" class="text-muted-foreground">
					View Process
				</Button>
			{/if}
		</div>

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
			{#if task.status === 'pending' && task.steps?.length}
				<!-- Pending with steps: show interactive form -->
				<div class="rounded-xl border border-border bg-card">
					<div class="border-b border-border p-4">
						<div class="flex items-center gap-2">
							<h1 class="text-lg font-semibold text-foreground">{task.title}</h1>
							<Badge class="bg-amber-100 text-amber-700" variant="secondary">
								Pending
							</Badge>
						</div>
						{#if task.instructions_mdsvex}
							<p class="mt-2 text-sm text-muted-foreground">{task.instructions_mdsvex}</p>
						{/if}
					</div>

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
	</div>
</div>

<SuccessOverlay visible={showSuccess} onDismiss={() => (showSuccess = false)} />
