<script lang="ts">
	import type { HumanTask, TaskStep } from '$lib/hpi/types';
	import { BlockRenderer, FieldDisplay } from '$lib/hpi';
	import type { ProcessState } from '$lib/hpi/types';
	import { StatusBadge } from '$lib/components/status';
	import Clock from '@lucide/svelte/icons/clock';
	import CheckCircle from '@lucide/svelte/icons/check-circle';
	import XCircle from '@lucide/svelte/icons/x-circle';
	import AlertCircle from '@lucide/svelte/icons/alert-circle';
	import User from '@lucide/svelte/icons/user';

	interface Props {
		task: HumanTask;
		process?: ProcessState | null;
		/** Base URL for HPI app (for "Open in HPI" link) */
		hpiBaseUrl?: string;
	}

	let { task, process = null, hpiBaseUrl }: Props = $props();

	// Leading icon per task state (colour + label come from the shared registry).
	const statusIcon = $derived(
		task.status === 'pending'
			? Clock
			: task.status === 'completed'
				? CheckCircle
				: task.status === 'failed'
					? AlertCircle
					: XCircle
	);
	const hasData = $derived(task.data && Object.keys(task.data).length > 0);

	function formatDuration(ms?: number | null): string | null {
		if (ms == null) return null;
		if (ms < 1000) return `${ms}ms`;
		const seconds = Math.floor(ms / 1000);
		if (seconds < 60) return `${(ms / 1000).toFixed(1)}s`;
		const minutes = Math.floor(seconds / 60);
		const remSec = seconds % 60;
		if (minutes < 60) return `${minutes}m ${remSec}s`;
		const hours = Math.floor(minutes / 60);
		const remMin = minutes % 60;
		if (hours < 24) return `${hours}h ${remMin}m`;
		const days = Math.floor(hours / 24);
		const remHours = hours % 24;
		return `${days}d ${remHours}h`;
	}

	const durationText = $derived(formatDuration(task.duration_ms));

	function getInputFields(steps: TaskStep[]) {
		return steps.flatMap((step) =>
			step.blocks.filter((b): b is Extract<typeof b, { type: 'input' }> => b.type === 'input')
		);
	}

	const inputFields = $derived(getInputFields(task.steps ?? []));
</script>

<div class="flex flex-col gap-3 p-3">
	<!-- Header -->
	<div class="flex items-start justify-between gap-2">
		<div class="min-w-0">
			<h3 class="text-sm font-semibold text-foreground truncate">{task.title}</h3>
			<div class="flex items-center gap-2 mt-1">
				<StatusBadge domain="task" status={task.status} icon={statusIcon} class="rounded-full" />
				{#if task.assignee_id}
					<span class="flex items-center gap-1 text-sm text-muted-foreground">
						<User class="size-3" />
						{task.assignee_id}
					</span>
				{/if}
			</div>
		</div>
		{#if hpiBaseUrl}
			<a
				href="{hpiBaseUrl}/task/{task.task_id}"
				target="_blank"
				rel="noopener noreferrer"
				class="shrink-0 text-sm text-primary hover:underline"
			>
				Open in HPI
			</a>
		{/if}
	</div>

	<!-- Task content (display blocks) -->
	{#each task.steps ?? [] as step (step.id)}
		{#if step.blocks.length > 0}
			<div class="space-y-2">
				{#if (task.steps ?? []).length > 1}
					<div class="text-sm font-medium text-muted-foreground uppercase tracking-wider">
						{step.title}
					</div>
				{/if}
				{#each step.blocks as block}
					{#if block.type !== 'input' && block.type !== 'repeater'}
						<BlockRenderer {block} taskData={task.payload} />
					{/if}
				{/each}
			</div>
		{/if}
	{/each}

	<!-- Completed data -->
	{#if task.status === 'completed' && hasData}
		<div class="border-t border-border pt-3 space-y-2">
			<div class="text-sm font-medium text-muted-foreground uppercase tracking-wider">
				Submitted Data
			</div>
			{#each inputFields as inputBlock (inputBlock.field.name)}
				<FieldDisplay
					field={inputBlock.field}
					fieldValue={task.data?.[inputBlock.field.name]}
				/>
			{/each}
		</div>
	{/if}

	<!-- Metadata -->
	<dl class="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 border-t border-border pt-3 text-sm">
		<dt class="text-muted-foreground">Created</dt>
		<dd class="text-foreground">{new Date(task.created_at).toLocaleString()}</dd>
		{#if task.completed_at}
			<dt class="text-muted-foreground">Completed</dt>
			<dd class="text-foreground">{new Date(task.completed_at).toLocaleString()}</dd>
		{/if}
		{#if durationText}
			<dt class="text-muted-foreground">Duration</dt>
			<dd class="text-foreground">{durationText}</dd>
		{/if}
	</dl>
</div>
