<script lang="ts">
	// One-line query bar over the catalogue text DSL (query-language.ts):
	// mono input + parsed-term chips. The bar is presentation-only — it binds
	// the shared draft, parsing is debounced for validation display, and the
	// raw text is what gets applied. The helpers that used to live in
	// popovers here (fields / syntax / saved queries) live in EntriesRail.
	import {
		parseQuery,
		formatQuery,
		validateTerms,
		removeTerm,
		type ParseError,
		type QueryTerm
	} from './query-language';
	import type { EntriesQueryState } from './entries-query.svelte';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import Search from '@lucide/svelte/icons/search';

	let {
		entries,
		knownFields,
		datatypeNames = null
	}: {
		entries: EntriesQueryState;
		/** Known filter field names for validation; null = registry not loaded yet. */
		knownFields: Set<string> | null;
		/** Registered data-type names; null = registry not loaded yet (skip). */
		datatypeNames?: Set<string> | null;
	} = $props();

	// ── Debounced parse (validation display only — apply is explicit) ────────
	let parsed = $state<{ terms: QueryTerm[]; errors: ParseError[] }>({ terms: [], errors: [] });
	let parseDebounce: ReturnType<typeof setTimeout> | undefined;
	$effect(() => {
		const t = entries.draft;
		clearTimeout(parseDebounce);
		parseDebounce = setTimeout(() => (parsed = parseQuery(t)), 150);
		return () => clearTimeout(parseDebounce);
	});

	// Unknown-field messages keyed by term raw (semantic layer over the parse).
	const fieldErrors = $derived.by(() => {
		if (!knownFields) return new Map<string, string>();
		return new Map(
			validateTerms(parsed.terms, knownFields, datatypeNames).map((e) => [e.raw, e.message])
		);
	});

	function apply() {
		entries.apply(entries.draft);
	}

	function onKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter') {
			e.preventDefault();
			apply();
		}
	}

	/** Remove one chip (valid term or error token) and re-apply. */
	function removeChip(raw: string) {
		const p = parseQuery(entries.draft);
		const rest = [
			formatQuery(removeTerm(p.terms, raw)),
			...p.errors.filter((e) => e.raw !== raw).map((e) => e.raw)
		]
			.filter(Boolean)
			.join(' ');
		entries.apply(rest);
	}
</script>

<div class="space-y-2">
	<div class="flex flex-wrap items-center gap-2">
		<div class="relative min-w-[20rem] flex-1">
			<Search class="absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" />
			<Input
				type="text"
				placeholder={'format:csv col:email meta.num_rows>1000 "free text"…'}
				class="h-8 pl-8 font-mono text-sm"
				bind:value={entries.draft}
				onkeydown={onKeydown}
				data-testid="query-bar-input"
			/>
		</div>

		<Button variant="default" size="sm" class="h-8" onclick={apply} data-testid="query-bar-apply">
			Apply
		</Button>
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
