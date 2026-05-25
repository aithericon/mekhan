<script lang="ts">
	import { Button } from '$lib/components/ui/button';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import Plus from '@lucide/svelte/icons/plus';
	import Play from '@lucide/svelte/icons/play';
	import Pencil from '@lucide/svelte/icons/pencil';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import CheckCircle2 from '@lucide/svelte/icons/check-circle-2';
	import XCircle from '@lucide/svelte/icons/x-circle';
	import AlertCircle from '@lucide/svelte/icons/alert-circle';
	import HelpCircle from '@lucide/svelte/icons/help-circle';
	import {
		listTemplateTests,
		runTemplateTest,
		runAllTemplateTests,
		updateTemplateTest,
		deleteTemplateTest,
		type TemplateTest,
		type TemplateTestRun
	} from '$lib/api/client';
	import TestEditorDialog from './TestEditorDialog.svelte';
	import TestRunDetailSheet from './TestRunDetailSheet.svelte';

	type Props = {
		templateId: string;
		/// Optional `node_slug → form schema` map for nicer assertion / answer
		/// authoring. Passed through to the dialog.
		humanTaskSlugs?: string[];
	};

	let { templateId, humanTaskSlugs = [] }: Props = $props();

	let tests = $state<TemplateTest[]>([]);
	let loading = $state(true);
	let runningId = $state<string | null>(null);
	let runningAll = $state(false);
	let editing = $state<TemplateTest | null>(null);
	let creating = $state(false);
	let inspecting = $state<TemplateTest | null>(null);
	let error = $state<string | null>(null);

	// Most-recent run keyed by test id so the badge updates inline after
	// a one-off run without re-fetching the whole list.
	let recentRun = $state<Record<string, TemplateTestRun>>({});

	async function load() {
		loading = true;
		error = null;
		try {
			tests = await listTemplateTests(templateId);
		} catch (e) {
			const msg = e instanceof Error ? e.message : 'failed to load tests';
			// A 404 here typically means the running service doesn't yet have
			// the `/tests` route (the schema was regenerated, mekhan needs a
			// restart). Treat it as "no tests yet" so the empty state lands
			// cleanly; the more informative "service out of date" hint is
			// covered by the regular empty state copy.
			if (/\b404\b/.test(msg)) {
				tests = [];
			} else {
				error = msg;
			}
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		if (templateId) void load();
	});

	async function handleRunOne(test: TemplateTest) {
		runningId = test.id;
		error = null;
		try {
			const run = await runTemplateTest(templateId, test.id);
			recentRun = { ...recentRun, [test.id]: run };
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : 'run failed';
		} finally {
			runningId = null;
		}
	}

	async function handleRunAll() {
		runningAll = true;
		error = null;
		try {
			const result = await runAllTemplateTests(templateId, false);
			// Match runs back to tests by index (server returns runs in the
			// same iteration order as the test list filtered to enabled).
			const enabledTests = tests.filter((t) => t.enabled);
			const updated: Record<string, TemplateTestRun> = { ...recentRun };
			result.runs.forEach((run, idx) => {
				const t = enabledTests[idx];
				if (t) updated[t.id] = run;
			});
			recentRun = updated;
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : 'run-all failed';
		} finally {
			runningAll = false;
		}
	}

	async function handleToggleEnabled(test: TemplateTest, enabled: boolean) {
		try {
			await updateTemplateTest(templateId, test.id, { enabled });
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : 'update failed';
		}
	}

	async function handleDelete(test: TemplateTest) {
		if (!confirm(`Delete test '${test.name}'?`)) return;
		try {
			await deleteTemplateTest(templateId, test.id);
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : 'delete failed';
		}
	}

	function badgeFor(test: TemplateTest) {
		const recent = recentRun[test.id];
		const status =
			recent?.status ??
			(test.last_run_passed === true
				? 'passed'
				: test.last_run_passed === false
					? 'failed'
					: null);
		switch (status) {
			case 'passed':
				return { icon: CheckCircle2, class: 'text-emerald-600', label: 'passed' };
			case 'failed':
				return { icon: XCircle, class: 'text-red-600', label: 'failed' };
			case 'error':
				return { icon: AlertCircle, class: 'text-amber-600', label: 'error' };
			default:
				return { icon: HelpCircle, class: 'text-muted-foreground', label: 'not run' };
		}
	}
</script>

<div class="flex h-full flex-col text-sm" data-testid="tests-panel">
	<header class="flex items-center justify-between border-b border-border px-3 py-2">
		<div class="font-medium">Tests</div>
		<div class="flex items-center gap-2">
			<Button
				variant="outline"
				size="sm"
				disabled={runningAll || tests.length === 0}
				onclick={handleRunAll}
			>
				<Play class="mr-1 size-3.5" />
				{runningAll ? 'Running…' : 'Run all'}
			</Button>
			<Button variant="default" size="sm" onclick={() => (creating = true)}>
				<Plus class="mr-1 size-3.5" />
				New
			</Button>
		</div>
	</header>

	{#if loading}
		<div class="p-3 text-muted-foreground">Loading…</div>
	{:else if error}
		<div class="border-b border-amber-200 bg-amber-50 px-3 py-2 text-amber-800">
			{error}
		</div>
	{:else if tests.length === 0}
		<div class="p-4 text-muted-foreground">
			No tests yet. Click <strong>New</strong> to author one, or open a completed instance and use
			"Save as test".
		</div>
	{:else}
		<ul class="flex-1 divide-y divide-border overflow-y-auto">
			{#each tests as test (test.id)}
				{@const badge = badgeFor(test)}
				{@const Icon = badge.icon}
				<li class="flex items-center gap-3 px-3 py-2" data-testid="test-row">
					<Checkbox
						checked={test.enabled}
						onCheckedChange={(checked) =>
							handleToggleEnabled(test, checked === true)}
						aria-label="enabled"
					/>
					<Button
						variant="ghost"
						class="h-auto min-w-0 flex-1 justify-start px-2 py-1 text-left"
						onclick={() => (inspecting = test)}
						title="View run history"
					>
						<div class="min-w-0 flex-1">
							<div class="truncate font-medium">{test.name}</div>
							<div class="flex items-center gap-1.5 text-xs {badge.class}">
								<Icon class="size-3" />
								<span>{badge.label}</span>
								{#if test.last_run_against_version != null}
									<span class="text-muted-foreground"
										>· v{test.last_run_against_version}</span
									>
								{/if}
							</div>
						</div>
					</Button>
					<Button
						variant="ghost"
						size="sm"
						disabled={runningId === test.id || !test.enabled}
						onclick={() => handleRunOne(test)}
						title={test.enabled ? 'Run test' : 'Test is disabled'}
					>
						<Play class="size-3.5" />
					</Button>
					<Button
						variant="ghost"
						size="sm"
						onclick={() => (editing = test)}
						title="Edit"
					>
						<Pencil class="size-3.5" />
					</Button>
					<Button
						variant="ghost"
						size="sm"
						onclick={() => handleDelete(test)}
						title="Delete"
					>
						<Trash2 class="size-3.5" />
					</Button>
				</li>
			{/each}
		</ul>
	{/if}
</div>

<TestEditorDialog
	{templateId}
	{humanTaskSlugs}
	open={creating || editing !== null}
	test={editing}
	onclose={() => {
		creating = false;
		editing = null;
	}}
	onsaved={async () => {
		creating = false;
		editing = null;
		await load();
	}}
/>

<TestRunDetailSheet
	{templateId}
	open={inspecting !== null}
	test={inspecting}
	onclose={() => (inspecting = null)}
/>
