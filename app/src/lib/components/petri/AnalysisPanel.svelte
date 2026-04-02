<script lang="ts">
	import { AlertTriangle, AlertCircle, Info, CheckCircle, RefreshCw } from '@lucide/svelte';

	interface AnalysisIssue {
		level: string;
		code: string;
		message: string;
		node_id: string;
		node_type: string;
	}

	interface AnalysisReport {
		is_valid: boolean;
		summary: { error_count: number; warning_count: number; info_count: number };
		issues: AnalysisIssue[];
	}

	interface Props {
		report: AnalysisReport | null;
		onRefresh?: () => void;
		onSelectNode?: (nodeId: string, nodeType: string) => void;
	}

	let { report = null, onRefresh, onSelectNode }: Props = $props();

	const sortedIssues = $derived.by(() => {
		if (!report?.issues) return [];
		const order: Record<string, number> = { error: 0, warning: 1, info: 2 };
		return [...report.issues].sort((a, b) => (order[a.level] ?? 9) - (order[b.level] ?? 9));
	});

	function handleIssueClick(nodeId: string, nodeType: string) {
		onSelectNode?.(nodeId, nodeType);
	}
</script>

<div class="h-full overflow-hidden flex flex-col">
	<div class="px-3 py-2 border-b border-border bg-muted shrink-0">
		<div class="flex items-center justify-between">
			<h3 class="font-semibold text-foreground text-sm">Static Analysis</h3>
			<div class="flex items-center gap-2">
				{#if onRefresh}
					<button
						class="p-1 rounded hover:bg-accent transition-colors"
						onclick={onRefresh}
						aria-label="Refresh analysis"
					>
						<RefreshCw class="h-3 w-3 text-muted-foreground" />
					</button>
				{/if}
				{#if report}
					{#if report.is_valid}
						<CheckCircle class="w-4 h-4 text-green-500" />
					{:else}
						<AlertCircle class="w-4 h-4 text-red-500" />
					{/if}
				{/if}
			</div>
		</div>
		{#if report?.summary}
			<div class="flex gap-3 text-xs text-muted-foreground mt-1">
				<span class={report.summary.error_count > 0 ? 'text-red-600 font-medium' : ''}>
					{report.summary.error_count} errors
				</span>
				<span class={report.summary.warning_count > 0 ? 'text-amber-600' : ''}>
					{report.summary.warning_count} warnings
				</span>
				<span class={report.summary.info_count > 0 ? 'text-blue-600' : ''}>
					{report.summary.info_count} info
				</span>
			</div>
		{/if}
	</div>
	<div class="flex-1 overflow-y-auto p-2">
		{#if !report}
			<p class="text-sm text-muted-foreground p-2">Loading...</p>
		{:else if sortedIssues.length === 0}
			<div class="flex flex-col items-center justify-center py-8 text-center">
				<CheckCircle class="w-8 h-8 text-green-500 mb-2" />
				<p class="text-sm text-muted-foreground">No issues found</p>
				<p class="text-xs text-muted-foreground mt-1">Topology is valid</p>
			</div>
		{:else}
			<div class="space-y-1">
				{#each sortedIssues as issue (issue.node_id + issue.code)}
					<button
						class="w-full text-left p-2 rounded hover:bg-muted transition-colors border border-transparent hover:border-border"
						onclick={() => handleIssueClick(issue.node_id, issue.node_type)}
					>
						<div class="flex items-start gap-2">
							{#if issue.level === 'error'}
								<AlertCircle class="w-4 h-4 text-red-500 shrink-0 mt-0.5" />
							{:else if issue.level === 'warning'}
								<AlertTriangle class="w-4 h-4 text-amber-500 shrink-0 mt-0.5" />
							{:else}
								<Info class="w-4 h-4 text-blue-500 shrink-0 mt-0.5" />
							{/if}
							<div class="min-w-0 flex-1">
								<div class="flex items-center gap-2">
									<span
										class="text-[10px] font-mono px-1.5 py-0.5 rounded
										{issue.level === 'error'
											? 'bg-red-500/15 text-red-500'
											: issue.level === 'warning'
												? 'bg-amber-500/15 text-amber-500'
												: 'bg-blue-500/15 text-blue-500'}"
									>
										{issue.code}
									</span>
									<span class="text-[10px] text-muted-foreground">
										{issue.node_type}
									</span>
								</div>
								<div class="text-xs mt-1 text-foreground">{issue.message}</div>
							</div>
						</div>
					</button>
				{/each}
			</div>
		{/if}
	</div>
</div>
