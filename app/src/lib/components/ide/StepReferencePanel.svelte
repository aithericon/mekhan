<script lang="ts">
	// Read + click-to-insert authoring reference for a Python automated step.
	//
	// Renders the qualified refs in scope (clean-cut `<slug>.<field>` or
	// `input.<path>`) plus the SDK helpers the runner injects. When the
	// parent provides `oninsertref`, every chip becomes a button that drops
	// the Python access form (`token["<part>"]`...) at the active editor's
	// cursor — same scope source the canvas Decision branch picker uses.
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import RefPicker from '$lib/components/editor/panels/property-sections/RefPicker.svelte';

	type Props = {
		/** This node's in-scope refs, grouped by producer in render. */
		scope: ScopeEntry[];
		busy?: boolean;
		/** Edges into this step. >1 (non-Join) ⇒ unmerged fan-in. */
		incomingCount?: number;
		onRefresh?: () => void;
		/** When provided, every ref becomes a click-to-insert button. The
		 *  parent wires this to the active code editor's `insertAtCursor`. */
		oninsertref?: (snippet: string) => void;
	};

	let { scope, busy = false, incomingCount = 0, onRefresh, oninsertref }: Props = $props();

	const unmergedFanIn = $derived(incomingCount > 1);

	// Group entries by producer in stable first-seen order, with the synthetic
	// "Process" bucket pushed last (control/identity, not business data) —
	// mirrors the RefPicker grouping in the canvas Decision picker.
	type Group = { key: string; label: string; isProcess: boolean; entries: ScopeEntry[] };
	const groups = $derived.by(() => {
		const out: Group[] = [];
		for (const e of scope) {
			const key = `${e.nodeId} ${e.nodeLabel}`;
			let g = out.find((x) => x.key === key);
			if (!g) {
				g = { key, label: e.nodeLabel, isProcess: e.nodeLabel === 'Process', entries: [] };
				out.push(g);
			}
			g.entries.push(e);
		}
		return out.sort((a, b) => Number(a.isProcess) - Number(b.isProcess));
	});

	// The runner exposes each upstream `<slug>` as a Python global and
	// `token`/`input` for the slim control token, so a qualified ref
	// inserts as the literal attribute-access expression — no
	// `token[...]` wrapping. `input.<field>` stays addressed off the
	// control-token loader (which is also accessible as `input`).
	//   "review.invoice_amount" → review.invoice_amount
	//   "input.invoice_id"      → input.invoice_id
	function refToPythonAccess(qualified: string): string {
		return qualified;
	}

	function insert(entry: ScopeEntry) {
		oninsertref?.(refToPythonAccess(entry.qualified));
	}

	// The runner auto-imports the SDK and injects these into step scope
	// (executor PythonBackend runner template). Everything else is reachable
	// via `import aithericon`.
	const SDK_HELPERS: { sig: string; doc: string }[] = [
		{
			sig: 'token',
			doc: 'The accumulating workflow token — ready to use, no import. Read fields with the qualified paths above (e.g. token["review"]["invoice_amount"]).'
		},
		{
			sig: 'load_inputs()',
			doc: 'Typed helper that returns the inputs staged for this step (mirrors the qualified refs shown above).'
		},
		{
			sig: 'set_output(name, value)',
			doc: "Emit one field on this node's output port. Downstream steps borrow it as <slug>.<name>."
		},
		{ sig: 'log_info(msg, **fields)', doc: 'Structured info log. Extra kwargs become log fields.' },
		{ sig: 'log_warn(msg, **fields)', doc: 'Structured warning log.' },
		{ sig: 'log_error(msg, **fields)', doc: 'Structured error log.' },
		{ sig: 'log_debug(msg, **fields)', doc: 'Structured debug log.' },
		{ sig: 'update_progress(fraction, message=…)', doc: 'Report 0.0–1.0 progress.' },
		{ sig: 'define_phases([…])', doc: 'Declare named phases up front.' },
		{ sig: 'update_phase(name, status)', doc: 'Move a declared phase (e.g. "running", "done").' },
		{ sig: 'log_metric(name, value)', doc: 'Record a single numeric metric.' },
		{ sig: 'log_artifact(path, name=…)', doc: 'Attach a file produced by this step.' }
	];
</script>

<details class="group border-b border-border" open>
	<summary
		class="flex list-none cursor-pointer select-none items-center justify-between px-3 py-2 text-sm font-semibold uppercase tracking-wider text-muted-foreground hover:text-foreground [&::-webkit-details-marker]:hidden"
	>
		<span>Reference</span>
		<span class="text-muted-foreground transition-transform group-open:rotate-90">›</span>
	</summary>

	<div class="max-h-[45vh] space-y-4 overflow-y-auto px-3 pb-3">
		{#if unmergedFanIn}
			<div
				class="rounded-md border border-amber-300 bg-amber-50 px-2.5 py-2 text-sm leading-snug text-amber-900"
			>
				<span class="font-semibold">⚠ Unmerged fan-in ({incomingCount} inputs).</span>
				This step isn't a Parallel Join, so it <strong>runs once per upstream
				token</strong> — each run sees only that branch's data. The fields below
				are the <em>union across all branches</em>, not what's present in a
				single run. Insert a <strong>Parallel Join</strong> upstream to combine
				inputs into one token.
			</div>
		{/if}

		{#if scope.length > 0 && oninsertref}
			<!-- Compact searchable picker on top — same component the canvas's
			     Decision branch editor uses. Picking an entry inserts the
			     Python dict-access form at the active editor's cursor. -->
			<RefPicker
				{scope}
				placeholder="Insert variable…"
				onpick={(e) => insert(e)}
			/>
		{/if}

		<div class="space-y-1.5">
			<div class="flex items-center justify-between gap-2">
				<div class="text-sm font-medium text-foreground">
					In-scope refs
					<span class="font-normal text-muted-foreground">
						— qualified <code class="font-mono">&lt;slug&gt;.&lt;field&gt;</code> or
						<code class="font-mono">input.&lt;path&gt;</code>
					</span>
				</div>
				{#if onRefresh}
					<button
						type="button"
						class="shrink-0 rounded px-1.5 py-0.5 text-sm text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-50"
						disabled={busy}
						onclick={() => onRefresh?.()}
						title="Recompute scope from the live graph"
						data-testid="step-reference-refresh"
					>
						{busy ? 'Refreshing…' : 'Refresh'}
					</button>
				{/if}
			</div>
			{#if groups.length > 0}
				<ul class="space-y-3">
					{#each groups as g (g.key)}
						<li class="space-y-0.5">
							<div
								class="text-sm uppercase tracking-wider {g.isProcess
									? 'italic text-muted-foreground'
									: 'text-muted-foreground'}"
							>
								{g.label}
							</div>
							<ul class="space-y-0.5">
								{#each g.entries as e (e.qualified)}
									<li>
										{#if oninsertref}
											<button
												type="button"
												class="flex w-full items-baseline justify-between gap-2 rounded px-1.5 py-0.5 text-left text-sm transition-colors hover:bg-accent hover:text-foreground"
												onclick={() => insert(e)}
												title={`Insert ${refToPythonAccess(e.qualified)} at cursor`}
												data-testid="step-reference-entry"
											>
												<code class="font-mono text-foreground">{e.qualified}</code>
												<span class="shrink-0 text-sm text-muted-foreground">{e.kind}</span>
											</button>
										{:else}
											<div class="flex items-baseline justify-between gap-2 text-sm">
												<code class="font-mono text-foreground">{e.qualified}</code>
												<span class="shrink-0 text-sm text-muted-foreground">{e.kind}</span>
											</div>
										{/if}
									</li>
								{/each}
							</ul>
						</li>
					{/each}
				</ul>
			{:else}
				<p class="text-sm text-muted-foreground">
					No upstream fields reach this step yet. Wire a Start or AutomatedStep upstream and
					declare its output port to reference fields here.
				</p>
				<p class="text-sm text-muted-foreground/70">
					Dynamic access always works at runtime: <code class="font-mono">token["field"]</code>.
				</p>
			{/if}
		</div>

		<div class="space-y-1.5">
			<div class="text-sm font-medium text-foreground">
				SDK helpers <span class="font-normal text-muted-foreground">— injected, no import</span>
			</div>
			<ul class="space-y-1.5">
				{#each SDK_HELPERS as h (h.sig)}
					<li class="text-sm">
						<code class="font-mono text-foreground">{h.sig}</code>
						<p class="mt-0.5 text-sm leading-snug text-muted-foreground">{h.doc}</p>
					</li>
				{/each}
			</ul>
		</div>

		<p class="text-sm leading-snug text-muted-foreground">
			Don't call <code class="font-mono">aithericon.init()</code> / <code class="font-mono">shutdown()</code> —
			the runner manages the IPC lifecycle. Fields update on publish from the live graph.
		</p>
	</div>
</details>
