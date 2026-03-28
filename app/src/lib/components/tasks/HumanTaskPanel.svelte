<script lang="ts">
	import type { HumanTask, TaskStep } from '@aithericon/hpi-ui/types';
	import { BlockRenderer, FieldDisplay, ProcessBanner } from '@aithericon/hpi-ui';
	import type { ProcessState } from '@aithericon/hpi-ui/types';
	import { Badge } from '$lib/components/ui/badge';
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

	const statusConfig: Record<string, { color: string; label: string }> = {
		pending: { color: 'bg-amber-100 text-amber-700', label: 'Pending' },
		completed: { color: 'bg-green-100 text-green-700', label: 'Completed' },
		cancelled: { color: 'bg-slate-100 text-slate-700', label: 'Cancelled' },
		failed: { color: 'bg-red-100 text-red-700', label: 'Failed' }
	};

	const status = $derived(statusConfig[task.status] ?? statusConfig.pending);
	const hasData = $derived(task.data && Object.keys(task.data).length > 0);

	function getInputFields(steps: TaskStep[]) {
		return steps.flatMap((step) =>
			step.blocks.filter((b): b is Extract<typeof b, { type: 'input' }> => b.type === 'input')
		);
	}

	const inputFields = $derived(getInputFields(task.steps));
</script>

<div class="flex flex-col gap-3 p-3">
	<!-- Header -->
	<div class="flex items-start justify-between gap-2">
		<div class="min-w-0">
			<h3 class="text-sm font-semibold text-foreground truncate">{task.title}</h3>
			<div class="flex items-center gap-2 mt-1">
				<Badge class={status.color} variant="secondary">
					{#if task.status === 'pending'}
						<Clock class="size-3 mr-1" />
					{:else if task.status === 'completed'}
						<CheckCircle class="size-3 mr-1" />
					{:else if task.status === 'failed'}
						<AlertCircle class="size-3 mr-1" />
					{:else}
						<XCircle class="size-3 mr-1" />
					{/if}
					{status.label}
				</Badge>
				{#if task.assignee_id}
					<span class="flex items-center gap-1 text-[11px] text-muted-foreground">
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
				class="shrink-0 text-xs text-primary hover:underline"
			>
				Open in HPI
			</a>
		{/if}
	</div>

	<!-- Process banner -->
	{#if process}
		<ProcessBanner {process} {task} />
	{/if}

	<!-- Task content (display blocks) -->
	{#each task.steps as step (step.id)}
		{#if step.blocks.length > 0}
			<div class="space-y-2">
				{#if task.steps.length > 1}
					<div class="text-xs font-medium text-muted-foreground uppercase tracking-wider">
						{step.title}
					</div>
				{/if}
				{#each step.blocks as block}
					{#if block.type !== 'input'}
						<BlockRenderer {block} />
					{/if}
				{/each}
			</div>
		{/if}
	{/each}

	<!-- Completed data -->
	{#if task.status === 'completed' && hasData}
		<div class="border-t border-border pt-3 space-y-2">
			<div class="text-xs font-medium text-muted-foreground uppercase tracking-wider">
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
	<div class="border-t border-border pt-2 text-[10px] text-muted-foreground">
		<span>Created {new Date(task.created_at).toLocaleString()}</span>
		{#if task.completed_at}
			<span class="ml-2">Completed {new Date(task.completed_at).toLocaleString()}</span>
		{/if}
		{#if task.duration_ms}
			<span class="ml-2">({(task.duration_ms / 1000).toFixed(1)}s)</span>
		{/if}
	</div>
</div>
