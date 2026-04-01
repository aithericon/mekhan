<script lang="ts">
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';
	import * as Select from '$lib/components/ui/select';

	type Props = {
		severity: 'info' | 'warning' | 'error' | 'success';
		title?: string;
		content: string;
		readonly?: boolean;
		onchange: (block: {
			severity: 'info' | 'warning' | 'error' | 'success';
			title?: string;
			content: string;
		}) => void;
		onremove: () => void;
	};

	let {
		severity,
		title,
		content,
		readonly = false,
		onchange,
		onremove
	}: Props = $props();

	const borderColors: Record<string, string> = {
		info: 'border-l-blue-400',
		warning: 'border-l-amber-400',
		error: 'border-l-red-400',
		success: 'border-l-green-400'
	};

	const badgeColors: Record<string, string> = {
		info: 'bg-blue-100 text-blue-700 dark:bg-blue-900/30 dark:text-blue-300',
		warning: 'bg-amber-100 text-amber-700 dark:bg-amber-900/30 dark:text-amber-300',
		error: 'bg-red-100 text-red-700 dark:bg-red-900/30 dark:text-red-300',
		success: 'bg-green-100 text-green-700 dark:bg-green-900/30 dark:text-green-300'
	};

	const severityLabels: Record<string, string> = {
		info: 'Info',
		warning: 'Warning',
		error: 'Error',
		success: 'Success'
	};
</script>

<div
	class="rounded-md border border-border/50 border-l-2 bg-background p-3 {borderColors[severity]}"
>
	<div class="mb-2 flex items-center justify-between">
		<span class="rounded px-2 py-0.5 text-xs font-medium {badgeColors[severity]}">
			Callout
		</span>
		{#if !readonly}
			<button
				type="button"
				class="rounded p-1 text-muted-foreground transition-colors hover:text-destructive"
				onclick={onremove}
			>
				<Trash2 class="size-4" />
			</button>
		{/if}
	</div>

	<div class="space-y-2">
		<Select.Root
			type="single"
			value={severity}
			onValueChange={(v) => {
				if (v)
					onchange({
						severity: v as typeof severity,
						title,
						content
					});
			}}
			disabled={readonly}
		>
			<Select.Trigger disabled={readonly} class="h-9 px-2 text-sm">
				{severityLabels[severity] ?? severity}
			</Select.Trigger>
			<Select.Content>
				<Select.Item value="info" label="Info" />
				<Select.Item value="warning" label="Warning" />
				<Select.Item value="error" label="Error" />
				<Select.Item value="success" label="Success" />
			</Select.Content>
		</Select.Root>

		<Input
			type="text"
			value={title ?? ''}
			placeholder="Title (optional)"
			disabled={readonly}
			oninput={(e) =>
				onchange({
					severity,
					title: (e.currentTarget as HTMLInputElement).value || undefined,
					content
				})}
		/>

		<Textarea
			value={content}
			placeholder="Callout message..."
			disabled={readonly}
			oninput={(e) =>
				onchange({
					severity,
					title,
					content: (e.currentTarget as HTMLTextAreaElement).value
				})}
			rows={3}
		/>
	</div>
</div>
