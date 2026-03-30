<script lang="ts">
	import { page } from '$app/stores';
	import { goto } from '$app/navigation';
	import { getTask, completeTask, cancelTask, getProcess } from '$lib/api/client';
	import type { HumanTask } from '$lib/types/tasks';
	import type { MekhanProcessState } from '$lib/types/tasks';
	import { HumanTaskPanel, TaskForm } from '$lib/components/tasks';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import ArrowLeft from '@lucide/svelte/icons/arrow-left';

	let task: HumanTask | null = $state(null);
	let process: MekhanProcessState | null = $state(null);
	let loading = $state(true);
	let error: string | null = $state(null);
	let submitting = $state(false);

	const taskId = $derived($page.params.id as string);

	async function load() {
		loading = true;
		error = null;
		try {
			task = await getTask(taskId);
			// Load process context if available
			if (task.process_id) {
				try {
					process = await getProcess(task.process_id);
				} catch {
					// Process may not exist in mekhan's index yet
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
			// Reload to show completed state
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
		if (reason === null) return; // User pressed Cancel on prompt
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
		taskId; // Track reactive dependency
		load();
	});
</script>

<div class="h-full overflow-y-auto">
	<div class="mx-auto max-w-3xl px-6 py-8 animate-rise">
		<!-- Back link -->
		<div class="mb-4">
			<Button variant="ghost" size="sm" href="/tasks" class="gap-1 text-muted-foreground">
				<ArrowLeft class="size-4" />
				Back to tasks
			</Button>
		</div>

		{#if error}
			<div
				class="mb-4 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800"
			>
				{error}
			</div>
		{/if}

		{#if loading}
			<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
				Loading...
			</div>
		{:else if task}
			{#if task.status === 'pending'}
				<!-- Pending: show interactive form -->
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
							onsubmit={handleComplete}
							oncancel={handleCancel}
							{submitting}
						/>
					</div>
				</div>
			{:else}
				<!-- Terminal state: read-only view -->
				<div class="rounded-xl border border-border bg-card">
					<HumanTaskPanel {task} process={process as any} />
				</div>
			{/if}
		{:else}
			<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
				Task not found
			</div>
		{/if}
	</div>
</div>
