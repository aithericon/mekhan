<script lang="ts">
	import {
		Sheet,
		SheetContent,
		SheetTitle,
		SheetDescription
	} from '$lib/components/ui/sheet';
	import { Button } from '$lib/components/ui/button';
	import { Card, CardContent } from '$lib/components/ui/card';
	import ExternalLink from '@lucide/svelte/icons/external-link';
	import CheckCircle2 from '@lucide/svelte/icons/check-circle-2';
	import XCircle from '@lucide/svelte/icons/x-circle';
	import AlertCircle from '@lucide/svelte/icons/alert-circle';
	import HelpCircle from '@lucide/svelte/icons/help-circle';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import {
		listTestRuns,
		type TemplateTest,
		type TemplateTestRun
	} from '$lib/api/client';

	type Props = {
		templateId: string;
		open: boolean;
		test: TemplateTest | null;
		onclose: () => void;
	};

	let { templateId, open, test, onclose }: Props = $props();

	let runs = $state<TemplateTestRun[]>([]);
	let loading = $state(false);
	let error = $state<string | null>(null);
	let scopeOpen = $state<Record<string, boolean>>({});

	function toggleScope(runId: string) {
		scopeOpen = { ...scopeOpen, [runId]: !scopeOpen[runId] };
	}

	// Re-fetch whenever the sheet opens on a new test.
	$effect(() => {
		if (!open || !test) {
			runs = [];
			error = null;
			return;
		}
		void loadRuns(test.id);
	});

	async function loadRuns(testId: string) {
		loading = true;
		error = null;
		try {
			runs = await listTestRuns(templateId, testId, 5);
		} catch (e) {
			error = e instanceof Error ? e.message : 'failed to load runs';
		} finally {
			loading = false;
		}
	}

	function statusIcon(status: string) {
		switch (status) {
			case 'passed':
				return { icon: CheckCircle2, class: 'text-emerald-600' };
			case 'failed':
				return { icon: XCircle, class: 'text-red-600' };
			case 'error':
				return { icon: AlertCircle, class: 'text-amber-600' };
			default:
				return { icon: HelpCircle, class: 'text-muted-foreground' };
		}
	}

	function formatJson(value: unknown): string {
		if (value === null || value === undefined) return '—';
		return JSON.stringify(value, null, 2);
	}

	function durationLabel(ms: number | null | undefined): string {
		if (ms == null) return '—';
		if (ms < 1000) return `${ms} ms`;
		return `${(ms / 1000).toFixed(2)} s`;
	}

	// `failure_detail` shape from the runner:
	//   failed:  { assertion_idx, path, op, expected, expected_resolved?, actual }
	//   error:   { assertion_idx?, path?, op?, expected?, error } OR
	//            { reason: 'instance_did_not_complete', terminal_status } OR
	//            { reason: 'launch_failed', detail: <launcher error string> }.
	//
	// `expected_resolved` is present only when the literal `expected` value was
	// a `{{ … }}` Rhai template that resolved to a different concrete value —
	// otherwise it'd just duplicate `expected`.
	type FailureDetail = {
		assertion_idx?: number;
		path?: string;
		op?: string;
		expected?: unknown;
		expected_resolved?: unknown;
		actual?: unknown;
		error?: string;
		reason?: string;
		detail?: string;
		terminal_status?: string;
	};

	function asFailureDetail(detail: unknown): FailureDetail | null {
		if (detail && typeof detail === 'object') return detail as FailureDetail;
		return null;
	}
</script>

<Sheet.Root
	{open}
	onOpenChange={(o: boolean) => {
		if (!o) onclose();
	}}
>
	<SheetContent class="flex w-full max-w-2xl flex-col gap-0 p-0 sm:max-w-2xl">
		<header class="border-b border-border px-5 py-4">
			<SheetTitle>
				{test ? `Test runs · ${test.name}` : 'Test runs'}
			</SheetTitle>
			<SheetDescription class="text-sm text-muted-foreground">
				Most recent execution history. Each run spawns a workflow
				instance in <code>test_run</code> mode.
			</SheetDescription>
		</header>

		<div class="flex-1 space-y-3 overflow-y-auto px-5 py-4 text-sm">
			{#if loading}
				<div class="text-muted-foreground">Loading…</div>
			{:else if error}
				<div
					class="rounded border border-red-200 bg-red-50 px-3 py-2 text-red-800"
				>
					{error}
				</div>
			{:else if runs.length === 0}
				<div class="text-muted-foreground">
					This test hasn't run yet. Hit the play button on the row.
				</div>
			{:else}
				{#each runs as run, idx (run.id)}
					{@const sig = statusIcon(run.status)}
					{@const Icon = sig.icon}
					{@const detail = asFailureDetail(run.failure_detail)}
					{@const isLatest = idx === 0}
					{@const isScopeOpen = scopeOpen[run.id] ?? false}
					<Card
						class={run.status === 'failed'
							? 'border-red-200'
							: run.status === 'error'
								? 'border-amber-200'
								: 'border-border'}
					>
						<CardContent class="space-y-2 p-3">
							<div class="flex items-center gap-2">
								<Icon class="size-4 {sig.class}" />
								<span class="font-medium {sig.class}">{run.status}</span>
								<span class="text-xs text-muted-foreground">
									v{run.template_version} · {durationLabel(run.duration_ms)}
								</span>
								{#if isLatest}
									<span
										class="ml-auto rounded-sm bg-muted px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground"
									>
										latest
									</span>
								{/if}
							</div>

							{#if detail}
								<div class="space-y-1 text-xs">
									{#if detail.reason === 'launch_failed'}
										<div>
											The runner couldn't launch the workflow instance —
											no engine activity occurred.
										</div>
										{#if detail.detail}
											<pre
												class="mt-1 max-h-40 overflow-auto whitespace-pre-wrap rounded bg-amber-50 p-2 font-mono text-[11px] text-amber-900">{detail.detail}</pre>
										{/if}
									{:else if detail.reason === 'instance_did_not_complete'}
										<div>
											The workflow instance terminated with status
											<code>{detail.terminal_status}</code> before any
											assertion could run.
										</div>
									{:else if typeof detail.assertion_idx === 'number'}
										<div>
											Assertion <strong>#{detail.assertion_idx + 1}</strong>
											{#if detail.path}
												on <code>{detail.path}</code>
											{/if}
											{#if detail.op}
												({detail.op})
											{/if}
										</div>
										{#if 'expected' in detail}
											<div class="flex gap-2">
												<span class="text-muted-foreground">expected:</span>
												<code class="break-all">
													{formatJson(detail.expected)}
												</code>
											</div>
										{/if}
										{#if 'expected_resolved' in detail}
											<div class="flex gap-2">
												<span class="text-muted-foreground">
													→ resolved:
												</span>
												<code class="break-all text-foreground">
													{formatJson(detail.expected_resolved)}
												</code>
											</div>
										{/if}
										{#if 'actual' in detail}
											<div class="flex gap-2">
												<span class="text-muted-foreground">actual:</span>
												<code class="break-all">
													{formatJson(detail.actual)}
												</code>
											</div>
										{/if}
										{#if detail.error}
											<div class="text-amber-700">{detail.error}</div>
										{/if}
									{:else if detail.error}
										<div class="text-amber-700">{detail.error}</div>
									{:else if detail.reason}
										<div class="text-muted-foreground">{detail.reason}</div>
									{/if}
								</div>
							{/if}

							<div class="flex items-center gap-2">
								<Button
									variant="outline"
									size="sm"
									href="/instances/{run.instance_id}"
								>
									<ExternalLink class="mr-1 size-3" />
									Open test instance
								</Button>
								{#if run.final_scope}
									<Button
										variant="ghost"
										size="sm"
										onclick={() => toggleScope(run.id)}
									>
										{#if isScopeOpen}
											<ChevronDown class="mr-1 size-3" />
										{:else}
											<ChevronRight class="mr-1 size-3" />
										{/if}
										final_scope
									</Button>
								{/if}
							</div>

							{#if run.final_scope && isScopeOpen}
								<pre
									class="max-h-64 overflow-auto rounded bg-muted/40 p-2 font-mono text-[11px]">{formatJson(
										run.final_scope
									)}</pre>
							{/if}
						</CardContent>
					</Card>
				{/each}
			{/if}
		</div>
	</SheetContent>
</Sheet.Root>
