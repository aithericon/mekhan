<script lang="ts">
	// Read-only authoring reference for a Python automated step: the qualified
	// fields readable here (this node's in-scope refs from the same
	// `/api/analyze` source the Decision branch picker uses) plus the SDK
	// helpers the runner injects.
	//
	// Renders `<slug>.<field>` / `input.<path>` qualified identifiers — the
	// clean-cut model. The Python runner still injects a `token` object, so a
	// footer reminds authors `token["field"]` works for dynamic access.
	import type { ScopeEntry } from '$lib/editor/guard-scope';

	type Props = {
		/** This node's in-scope refs, grouped by producer in render. */
		scope: ScopeEntry[];
		busy?: boolean;
		/** Edges into this step. >1 (non-Join) ⇒ unmerged fan-in. */
		incomingCount?: number;
		onRefresh?: () => void;
	};

	let { scope, busy = false, incomingCount = 0, onRefresh }: Props = $props();

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
									<li class="flex items-baseline justify-between gap-2 text-sm">
										<code class="font-mono text-foreground">{e.qualified}</code>
										<span class="shrink-0 text-sm text-muted-foreground">{e.kind}</span>
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
