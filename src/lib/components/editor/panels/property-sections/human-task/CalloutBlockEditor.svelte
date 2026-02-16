<script lang="ts">
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import { Select, SelectTrigger, SelectContent, SelectItem } from '$lib/components/ui/select';

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
	class="rounded border border-border/50 border-l-2 bg-background p-1.5 {borderColors[severity]}"
>
	<div class="mb-1.5 flex items-center justify-between">
		<span class="rounded px-1.5 py-0.5 text-[9px] font-medium {badgeColors[severity]}">
			Callout
		</span>
		{#if !readonly}
			<button
				type="button"
				class="rounded p-0.5 text-muted-foreground transition-colors hover:text-destructive"
				onclick={onremove}
			>
				<Trash2 class="size-3" />
			</button>
		{/if}
	</div>

	<div class="space-y-1.5">
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
			<SelectTrigger disabled={readonly} class="h-5 px-1 py-0 text-[10px]">
				{severityLabels[severity] ?? severity}
			</SelectTrigger>
			<SelectContent>
				<SelectItem value="info" label="Info" />
				<SelectItem value="warning" label="Warning" />
				<SelectItem value="error" label="Error" />
				<SelectItem value="success" label="Success" />
			</SelectContent>
		</Select.Root>

		<input
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
			class="w-full rounded border border-input bg-background px-1.5 py-0.5 text-[10px] focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
		/>

		<textarea
			value={content}
			placeholder="Callout message..."
			disabled={readonly}
			oninput={(e) =>
				onchange({
					severity,
					title,
					content: (e.currentTarget as HTMLTextAreaElement).value
				})}
			rows={2}
			class="w-full rounded border border-input bg-background px-1.5 py-1 text-[10px] text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
		></textarea>
	</div>
</div>
