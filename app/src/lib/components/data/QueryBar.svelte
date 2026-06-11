<script lang="ts">
	// One-line query bar over the catalogue text DSL (query-language.ts):
	// mono input + parsed-term chips + three helpers (Fields picker / syntax
	// Help / Saved queries). The bar itself is presentation-only — parsing is
	// debounced for validation display, the raw text is what gets applied.
	import {
		parseQuery,
		formatQuery,
		compileQuery,
		validateTerms,
		removeTerm,
		addTerm,
		type ParseError,
		type QueryTerm
	} from './query-language';
	import {
		getCatalogueQueryFields,
		listSavedQueries,
		createSavedQuery,
		deleteSavedQuery,
		type QueryFieldsResponse,
		type SavedQuery
	} from '$lib/api/data';
	import { ApiError } from '$lib/api/client';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import * as Popover from '$lib/components/ui/popover';
	import { toast } from 'svelte-sonner';
	import Search from '@lucide/svelte/icons/search';
	import ListFilter from '@lucide/svelte/icons/list-filter';
	import CircleHelp from '@lucide/svelte/icons/circle-help';
	import Bookmark from '@lucide/svelte/icons/bookmark';
	import X from '@lucide/svelte/icons/x';

	let {
		value,
		onApply,
		knownFields
	}: {
		/** The applied query text (source of truth lives in the parent). */
		value: string;
		onApply: (text: string) => void;
		/** Known filter field names for validation; null = registry not loaded yet. */
		knownFields: Set<string> | null;
	} = $props();

	// Local draft text — re-synced whenever the parent applies a new value
	// (facet click, chip removal, URL navigation). Starts empty; the sync
	// effect runs before first paint so the deep-linked value still hydrates.
	let text = $state('');
	$effect(() => {
		text = value;
	});

	let inputEl = $state<HTMLInputElement | null>(null);

	// ── Debounced parse (validation display only — apply is explicit) ────────
	let parsed = $state<{ terms: QueryTerm[]; errors: ParseError[] }>({ terms: [], errors: [] });
	let parseDebounce: ReturnType<typeof setTimeout> | undefined;
	$effect(() => {
		const t = text;
		clearTimeout(parseDebounce);
		parseDebounce = setTimeout(() => (parsed = parseQuery(t)), 150);
		return () => clearTimeout(parseDebounce);
	});

	// Unknown-field messages keyed by term raw (semantic layer over the parse).
	const fieldErrors = $derived.by(() => {
		if (!knownFields) return new Map<string, string>();
		return new Map(validateTerms(parsed.terms, knownFields).map((e) => [e.raw, e.message]));
	});

	function apply() {
		onApply(text);
	}

	function onKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter') {
			e.preventDefault();
			apply();
		}
	}

	/** Remove one chip (valid term or error token) and re-apply. */
	function removeChip(raw: string) {
		const p = parseQuery(text);
		const rest = [
			formatQuery(removeTerm(p.terms, raw)),
			...p.errors.filter((e) => e.raw !== raw).map((e) => e.raw)
		]
			.filter(Boolean)
			.join(' ');
		onApply(rest);
	}

	function insertTerm(term: string) {
		text = addTerm(text, term);
		inputEl?.focus();
	}

	// ── Fields registry (static per server build — module-cached fetch) ──────
	let registry = $state<QueryFieldsResponse | null>(null);
	$effect(() => {
		getCatalogueQueryFields()
			.then((r) => (registry = r))
			.catch(() => {});
	});

	// ── Saved queries ─────────────────────────────────────────────────────────
	let savedOpen = $state(false);
	let saved = $state<SavedQuery[]>([]);
	let savedLoading = $state(false);
	let saveName = $state('');
	let saving = $state(false);

	async function loadSaved() {
		savedLoading = true;
		try {
			saved = await listSavedQueries();
		} catch {
			saved = [];
		} finally {
			savedLoading = false;
		}
	}
	$effect(() => {
		if (savedOpen) loadSaved();
	});

	async function saveCurrent() {
		const name = saveName.trim();
		if (!name) return;
		saving = true;
		try {
			const compiled = compileQuery(parseQuery(text).terms);
			await createSavedQuery({ name, q: text, params: compiled });
			toast.success(`Saved query “${name}”`);
			saveName = '';
			await loadSaved();
		} catch (e) {
			if (e instanceof ApiError && e.status === 409) {
				toast.error(`A saved query named “${name}” already exists`);
			} else {
				toast.error(e instanceof Error ? e.message : 'Failed to save query');
			}
		} finally {
			saving = false;
		}
	}

	async function removeSaved(sq: SavedQuery) {
		if (!confirm(`Delete saved query “${sq.name}”?`)) return;
		try {
			await deleteSavedQuery(sq.id);
			saved = saved.filter((s) => s.id !== sq.id);
		} catch (e) {
			toast.error(e instanceof Error ? e.message : 'Failed to delete saved query');
		}
	}

	function applySaved(sq: SavedQuery) {
		savedOpen = false;
		onApply(sq.q);
	}

	// Static syntax cheat-sheet (Help popover).
	const HELP_ROWS: Array<[string, string]> = [
		['word · "free text"', 'free-text search over name / hash'],
		['field:value', 'equals · field!=value for not-equals'],
		['field:a,b,c', 'any of (unquoted comma list)'],
		['field:null · field:*', 'missing · present'],
		['size_bytes>10m', 'comparisons > >= < <= · byte suffixes k/m/g/t'],
		['created_at>-7d', 'relative dates m/h/d/w/y · or ISO dates'],
		['format:csv', 'file_metadata format'],
		['col:email · dim:time', 'has column · has dimension'],
		['pii:EMAIL', 'has a column classified as…'],
		['attr:KEY=VALUE', 'custom attribute'],
		['owner:"null"', 'quoting opts out of special forms']
	];
</script>

<div class="space-y-2">
	<div class="flex flex-wrap items-center gap-2">
		<div class="relative min-w-[20rem] flex-1">
			<Search class="absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" />
			<Input
				bind:ref={inputEl}
				type="text"
				placeholder={'format:csv col:email meta.num_rows>1000 "free text"…'}
				class="h-8 pl-8 font-mono text-sm"
				bind:value={text}
				onkeydown={onKeydown}
				data-testid="query-bar-input"
			/>
		</div>

		<Button variant="default" size="sm" class="h-8" onclick={apply} data-testid="query-bar-apply">
			Apply
		</Button>

		<!-- Fields picker -->
		<Popover.Root>
			<Popover.Trigger>
				{#snippet child({ props })}
					<Button {...props} variant="ghost" size="sm" class="h-8" data-testid="query-bar-fields">
						<ListFilter class="size-3.5" />
						Fields
					</Button>
				{/snippet}
			</Popover.Trigger>
			<Popover.Content align="end" class="max-h-96 w-96 overflow-y-auto p-0">
				{#if !registry}
					<p class="px-3 py-2 text-sm text-muted-foreground">Loading field registry…</p>
				{:else}
					{#each [{ label: 'Fields', items: registry.native }, { label: 'Metadata (meta.*)', items: registry.meta }] as group (group.label)}
						<div class="border-b border-border px-1 py-1 last:border-b-0">
							<p class="px-2 pb-1 pt-1.5 text-xs font-medium uppercase tracking-wide text-muted-foreground">
								{group.label}
							</p>
							{#each group.items as f (f.name)}
								<button
									type="button"
									class="flex w-full items-baseline gap-2 rounded px-2 py-1 text-left text-sm hover:bg-accent"
									onclick={() => insertTerm(`${f.name}:`)}
								>
									<span class="font-mono text-foreground">{f.name}</span>
									<span class="text-xs text-muted-foreground">{f.value_type}</span>
									<span class="ml-auto truncate text-xs text-muted-foreground" title={f.description}>
										{f.description}
									</span>
								</button>
							{/each}
						</div>
					{/each}
					<div class="px-1 py-1">
						<p class="px-2 pb-1 pt-1.5 text-xs font-medium uppercase tracking-wide text-muted-foreground">
							Metadata containment
						</p>
						{#each registry.containment as c (c.term)}
							<button
								type="button"
								class="flex w-full items-baseline gap-2 rounded px-2 py-1 text-left text-sm hover:bg-accent"
								onclick={() => insertTerm(`${c.term}:`)}
							>
								<span class="font-mono text-foreground">{c.term}:</span>
								<span class="ml-auto truncate text-xs text-muted-foreground" title={c.description}>
									{c.description}
								</span>
							</button>
						{/each}
					</div>
				{/if}
			</Popover.Content>
		</Popover.Root>

		<!-- Syntax help -->
		<Popover.Root>
			<Popover.Trigger>
				{#snippet child({ props })}
					<Button {...props} variant="ghost" size="sm" class="h-8" data-testid="query-bar-help">
						<CircleHelp class="size-3.5" />
						Help
					</Button>
				{/snippet}
			</Popover.Trigger>
			<Popover.Content align="end" class="w-96 p-3">
				<p class="pb-2 text-xs font-medium uppercase tracking-wide text-muted-foreground">
					Query syntax
				</p>
				<table class="w-full text-sm">
					<tbody>
						{#each HELP_ROWS as [syntax, meaning] (syntax)}
							<tr class="align-baseline">
								<td class="whitespace-nowrap pr-3 font-mono text-xs text-foreground">{syntax}</td>
								<td class="py-0.5 text-xs text-muted-foreground">{meaning}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</Popover.Content>
		</Popover.Root>

		<!-- Saved queries -->
		<Popover.Root bind:open={savedOpen}>
			<Popover.Trigger>
				{#snippet child({ props })}
					<Button {...props} variant="ghost" size="sm" class="h-8" data-testid="query-bar-saved">
						<Bookmark class="size-3.5" />
						Saved
					</Button>
				{/snippet}
			</Popover.Trigger>
			<Popover.Content align="end" class="w-80 p-0">
				<div class="max-h-64 overflow-y-auto px-1 py-1">
					{#if savedLoading}
						<p class="px-2 py-1.5 text-sm text-muted-foreground">Loading…</p>
					{:else if saved.length === 0}
						<p class="px-2 py-1.5 text-sm text-muted-foreground">No saved queries yet</p>
					{:else}
						{#each saved as sq (sq.id)}
							<div class="flex items-center gap-1 rounded px-1 hover:bg-accent">
								<button
									type="button"
									class="min-w-0 flex-1 px-1 py-1.5 text-left"
									title={sq.q}
									onclick={() => applySaved(sq)}
								>
									<span class="block truncate text-sm text-foreground">{sq.name}</span>
									<span class="block truncate font-mono text-xs text-muted-foreground">{sq.q}</span>
								</button>
								<button
									type="button"
									class="rounded p-1 text-muted-foreground hover:text-destructive"
									title="Delete saved query"
									onclick={() => removeSaved(sq)}
								>
									<X class="size-3.5" />
								</button>
							</div>
						{/each}
					{/if}
				</div>
				<div class="flex items-center gap-2 border-t border-border p-2">
					<Input
						type="text"
						placeholder="Save current as…"
						class="h-8 flex-1 text-sm"
						bind:value={saveName}
						onkeydown={(e) => {
							if (e.key === 'Enter') {
								e.preventDefault();
								saveCurrent();
							}
						}}
						data-testid="query-bar-save-name"
					/>
					<Button
						variant="secondary"
						size="sm"
						class="h-8"
						disabled={saving || !saveName.trim()}
						onclick={saveCurrent}
						data-testid="query-bar-save"
					>
						Save
					</Button>
				</div>
			</Popover.Content>
		</Popover.Root>
	</div>

	{#if parsed.terms.length > 0 || parsed.errors.length > 0}
		<div class="flex flex-wrap items-center gap-1.5" data-testid="query-bar-chips">
			{#each parsed.terms as term (term.raw + term.kind)}
				{@const fieldError = fieldErrors.get(term.raw)}
				<Badge
					variant="secondary"
					class={`gap-1 font-mono text-xs ${fieldError ? 'bg-rose-100 text-rose-800 dark:bg-rose-950 dark:text-rose-200' : ''}`}
					title={fieldError}
				>
					{term.raw}
					<button
						type="button"
						class="ml-0.5 hover:text-foreground"
						title="Remove term"
						onclick={() => removeChip(term.raw)}
					>
						&times;
					</button>
				</Badge>
			{/each}
			{#each parsed.errors as err (err.raw + err.index)}
				<Badge
					variant="destructive"
					class="gap-1 font-mono text-xs"
					title={err.message}
				>
					{err.raw}
					<button
						type="button"
						class="ml-0.5 hover:text-destructive-foreground"
						title="Remove term"
						onclick={() => removeChip(err.raw)}
					>
						&times;
					</button>
				</Badge>
			{/each}
		</div>
	{/if}
</div>
