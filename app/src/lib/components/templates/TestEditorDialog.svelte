<script lang="ts">
	import {
		Sheet,
		SheetContent,
		SheetTitle,
		SheetDescription
	} from '$lib/components/ui/sheet';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';
	import { Label } from '$lib/components/ui/label';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import * as Select from '$lib/components/ui/select';
	import { Card, CardContent, CardHeader, CardTitle } from '$lib/components/ui/card';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import {
		createTemplateTest,
		updateTemplateTest,
		type TemplateTest,
		type Assertion,
		type AssertOp
	} from '$lib/api/client';
	import RefPicker from '$lib/components/editor/panels/property-sections/RefPicker.svelte';
	import type { ScopeEntry } from '$lib/editor/guard-scope';

	type Props = {
		templateId: string;
		open: boolean;
		test: TemplateTest | null;
		/// HumanTask node slugs found in the template; rendered as a small
		/// reference list in the human_answers field so authors know which
		/// keys to populate.
		humanTaskSlugs?: string[];
		/// Synthetic ScopeEntry list for the assertion path picker. Built by
		/// the parent from each End node's `resultMapping` (see
		/// `$lib/editor/assertion-scope`). Empty array → picker disables.
		assertionScope?: ScopeEntry[];
		onclose: () => void;
		onsaved: () => void;
	};

	let {
		templateId,
		open,
		test,
		humanTaskSlugs = [],
		assertionScope = [],
		onclose,
		onsaved
	}: Props = $props();

	let name = $state('');
	let enabled = $state(true);
	let startTokensText = $state('[]');
	// One textarea per HumanTask slug in the template graph. Stored on disk
	// as a single `{ <slug>: <answers> }` object, but authored per-slug so
	// templates with multiple HumanTasks (or zero) get the right UI.
	let humanAnswersBySlug = $state<Record<string, string>>({});
	let assertions = $state<Assertion[]>([]);
	let saving = $state(false);
	let error = $state<string | null>(null);
	let scopeOpen = $state(true);

	$effect(() => {
		if (!open) {
			error = null;
			return;
		}
		// Seed editor fields whenever a different test (or "new test")
		// becomes the target. The incoming `test` is already wrapped in
		// Svelte's deep proxy (it lives in TestsPanel's `$state`), so
		// `structuredClone` would throw — round-trip through JSON to get a
		// plain, owned copy we can mutate freely without writing back to
		// the parent.
		if (test) {
			name = test.name;
			enabled = test.enabled;
			startTokensText = JSON.stringify(test.start_tokens ?? [], null, 2);
			const stored = (test.human_answers ?? {}) as Record<string, unknown>;
			const seeded: Record<string, string> = {};
			for (const slug of humanTaskSlugs) {
				seeded[slug] = JSON.stringify(stored[slug] ?? {}, null, 2);
			}
			humanAnswersBySlug = seeded;
			assertions = Array.isArray(test.assertions)
				? (JSON.parse(JSON.stringify(test.assertions)) as Assertion[])
				: [];
		} else {
			name = '';
			enabled = true;
			startTokensText = '[]';
			const seeded: Record<string, string> = {};
			for (const slug of humanTaskSlugs) {
				seeded[slug] = '{}';
			}
			humanAnswersBySlug = seeded;
			assertions = [];
		}
	});

	const OPS: AssertOp[] = [
		'eq',
		'neq',
		'exists',
		'not_exists',
		'gt',
		'gte',
		'lt',
		'lte',
		'matches',
		'contains'
	];

	function addAssertion() {
		assertions = [
			...assertions,
			{ path: 'result.value.', op: 'eq' as AssertOp, value: '' }
		];
	}

	function removeAssertion(idx: number) {
		assertions = assertions.filter((_, i) => i !== idx);
	}

	function updateAssertion(idx: number, patch: Partial<Assertion>) {
		assertions = assertions.map((a, i) => (i === idx ? { ...a, ...patch } : a));
	}

	function valueNeedsRhs(op: AssertOp): boolean {
		return op !== 'exists' && op !== 'not_exists';
	}

	function parseRhs(raw: unknown): unknown {
		if (typeof raw !== 'string') return raw;
		const trimmed = raw.trim();
		if (trimmed === '') return '';
		// Best-effort: try JSON first (numbers, booleans, arrays, objects),
		// fall back to the raw string so plain text doesn't need quoting.
		try {
			return JSON.parse(trimmed);
		} catch {
			return raw;
		}
	}

	async function handleSave() {
		saving = true;
		error = null;
		try {
			const startTokens = JSON.parse(startTokensText);
			const humanAnswers: Record<string, unknown> = {};
			for (const slug of humanTaskSlugs) {
				const raw = humanAnswersBySlug[slug] ?? '{}';
				try {
					humanAnswers[slug] = JSON.parse(raw);
				} catch {
					throw new Error(`Invalid JSON in answers for '${slug}'`);
				}
			}
			const cleanedAssertions = assertions.map((a) => ({
				...a,
				value: valueNeedsRhs(a.op) ? parseRhs(a.value) : null
			}));
			if (test) {
				await updateTemplateTest(templateId, test.id, {
					name,
					enabled,
					start_tokens: startTokens,
					human_answers: humanAnswers,
					assertions: cleanedAssertions
				});
			} else {
				await createTemplateTest(templateId, {
					name,
					enabled,
					start_tokens: startTokens,
					human_answers: humanAnswers,
					assertions: cleanedAssertions
				});
			}
			onsaved();
		} catch (e) {
			error = e instanceof Error ? e.message : 'save failed';
		} finally {
			saving = false;
		}
	}

	function rhsTextValue(value: unknown): string {
		if (value === null || value === undefined) return '';
		if (typeof value === 'string') return value;
		return JSON.stringify(value);
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
			<SheetTitle>{test ? `Edit test` : 'New test'}</SheetTitle>
			<SheetDescription class="text-sm text-muted-foreground">
				Fixed inputs + human answers + assertions. Runs against the latest published
				version.
			</SheetDescription>
		</header>

		<div class="flex-1 overflow-y-auto px-5 py-4">
			{#if error}
				<div
					class="mb-3 rounded border border-red-200 bg-red-50 px-3 py-2 text-sm text-red-800"
				>
					{error}
				</div>
			{/if}

			<div class="space-y-4 text-sm">
				{#if test?.reference_scope}
					<Card class="border-border bg-muted/30">
						<CardHeader class="p-3">
							<Button
								variant="ghost"
								size="sm"
								class="h-auto justify-start gap-1.5 px-1 py-0 text-left font-medium hover:bg-transparent"
								onclick={() => (scopeOpen = !scopeOpen)}
							>
								{#if scopeOpen}
									<ChevronDown class="size-3.5" />
								{:else}
									<ChevronRight class="size-3.5" />
								{/if}
								<CardTitle class="text-sm">Available scope</CardTitle>
								<span class="font-normal text-muted-foreground">
									— write assertions against these paths
								</span>
							</Button>
						</CardHeader>
						{#if scopeOpen}
							<CardContent class="px-3 pb-3 pt-0">
								<pre
									class="max-h-64 overflow-auto rounded bg-background/60 p-2 font-mono text-[11px] leading-snug">{JSON.stringify(
										test.reference_scope,
										null,
										2
									)}</pre>
							</CardContent>
						{/if}
					</Card>
				{/if}

				<div class="space-y-1">
					<Label for="test-name">Name</Label>
					<Input
						id="test-name"
						bind:value={name}
						placeholder="happy-path-approve"
					/>
				</div>

				<label class="flex items-center gap-2">
					<Checkbox
						checked={enabled}
						onCheckedChange={(v) => (enabled = v === true)}
					/>
					<span>Enabled</span>
				</label>

				<div class="space-y-1">
					<Label for="start-tokens">Start tokens</Label>
					<Textarea
						id="start-tokens"
						bind:value={startTokensText}
						class="h-32 font-mono text-xs"
					/>
					<p class="text-xs text-muted-foreground">
						JSON array of <code>{`{ start_block_id, token }`}</code> entries — same shape as
						<code>CreateInstanceRequest.start_tokens</code>.
					</p>
				</div>

				{#if humanTaskSlugs.length > 0}
					<div class="space-y-2">
						<Label>Human task answers</Label>
						<p class="text-xs text-muted-foreground">
							One block per HumanTask in this template. The runner publishes
							these as the synthetic completion payload when each task fires.
						</p>
						{#each humanTaskSlugs as slug (slug)}
							<div class="space-y-1">
								<Label for={`ans-${slug}`} class="font-mono text-xs">
									{slug}
								</Label>
								<Textarea
									id={`ans-${slug}`}
									value={humanAnswersBySlug[slug] ?? '{}'}
									oninput={(e) => {
										humanAnswersBySlug = {
											...humanAnswersBySlug,
											[slug]: (e.target as HTMLTextAreaElement).value
										};
									}}
									class="h-24 font-mono text-xs"
								/>
							</div>
						{/each}
					</div>
				{/if}

				<div class="space-y-2">
					<div class="flex items-center justify-between">
						<Label>Assertions</Label>
						<Button variant="outline" size="sm" onclick={addAssertion}>
							<Plus class="mr-1 size-3.5" /> Add
						</Button>
					</div>
					{#if assertions.length === 0}
						<p class="text-xs text-muted-foreground">
							Each assertion checks a value at a dot-path inside <code
								>{`{ result, steps.<slug>.output }`}</code
							>. Wrap an expected value in <code>{`{{ … }}`}</code> to evaluate
							it as a Rhai expression against the same scope.
						</p>
					{/if}
					{#each assertions as a, idx (idx)}
						<div class="flex items-start gap-2">
							<div class="flex flex-1 flex-col gap-1">
								<Input
									class="font-mono text-xs"
									placeholder="result.value.invoice_amount"
									bind:value={a.path}
									oninput={(e) =>
										updateAssertion(idx, {
											path: (e.target as HTMLInputElement).value
										})}
								/>
								<RefPicker
									scope={assertionScope}
									selected={a.path}
									placeholder={assertionScope.length === 0
										? 'Declare resultMapping on an End node'
										: 'Pick result field…'}
									onpick={(entry) =>
										updateAssertion(idx, { path: entry.qualified })}
								/>
							</div>
							<Select.Root
								type="single"
								value={a.op}
								onValueChange={(v) =>
									updateAssertion(idx, { op: v as AssertOp })}
							>
								<Select.Trigger class="w-28">
									{a.op}
								</Select.Trigger>
								<Select.Content>
									{#each OPS as op}
										<Select.Item value={op}>{op}</Select.Item>
									{/each}
								</Select.Content>
							</Select.Root>
							{#if valueNeedsRhs(a.op)}
								<div class="flex flex-1 flex-col gap-1">
									<Input
										class="font-mono text-xs"
										placeholder={'"yes" / 42 / {{ result.value.amount }}'}
										value={rhsTextValue(a.value)}
										oninput={(e) =>
											updateAssertion(idx, {
												value: (e.target as HTMLInputElement).value
											})}
									/>
									<RefPicker
										scope={assertionScope}
										placeholder={assertionScope.length === 0
											? 'No scope refs'
											: 'Insert {{ ref }}…'}
										onpick={(entry) =>
											updateAssertion(idx, {
												value: `{{ ${entry.qualified} }}`
											})}
									/>
								</div>
							{:else}
								<div class="flex-1 text-xs text-muted-foreground self-center pl-2">
									(no value)
								</div>
							{/if}
							<Button
								variant="ghost"
								size="sm"
								onclick={() => removeAssertion(idx)}
								title="Remove"
							>
								<Trash2 class="size-3.5" />
							</Button>
						</div>
					{/each}
				</div>
			</div>
		</div>

		<footer class="flex justify-end gap-2 border-t border-border px-5 py-3">
			<Button variant="outline" onclick={onclose} disabled={saving}>Cancel</Button>
			<Button onclick={handleSave} disabled={saving || !name.trim()}>
				{saving ? 'Saving…' : test ? 'Save changes' : 'Create test'}
			</Button>
		</footer>
	</SheetContent>
</Sheet.Root>
