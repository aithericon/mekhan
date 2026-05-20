<script lang="ts">
	// Read-only authoring reference for a Python automated step: the token
	// fields readable here (this node's input scope, the same `input.<field>`
	// set the generated `.pyi` types) plus the SDK helpers the runner injects.
	// Purely presentational — data comes from `getStepScopes()`.
	import type { StepScopeField } from '$lib/api/client';

	type Props = {
		/** This node's input scope: fields readable as `token.<name>`. */
		fields: StepScopeField[];
		/** Server diagnostic, so an empty list explains itself. */
		diagnostic?: string;
		busy?: boolean;
		/** Edges into this step. >1 (non-Join) ⇒ unmerged fan-in. */
		incomingCount?: number;
		onRefresh?: () => void;
	};

	let {
		fields,
		diagnostic = 'ok',
		busy = false,
		incomingCount = 0,
		onRefresh
	}: Props = $props();

	const unmergedFanIn = $derived(incomingCount > 1);

	// Turn the raw server diagnostic into a one-liner for the empty state.
	const emptyReason = $derived.by(() => {
		if (fields.length > 0) return null;
		if (diagnostic.startsWith('ydoc_unreadable'))
			return "Couldn't read the live graph — your edits aren't lost; try Refresh in a moment.";
		if (diagnostic.startsWith('graph_not_scopable'))
			return 'The graph isn’t a complete flow yet (missing Start, a cycle, or a dangling edge). Wire this step to an upstream node, then Refresh.';
		if (diagnostic.startsWith('request_failed'))
			return 'Scope request failed. Try Refresh.';
		if (diagnostic === 'no_ydoc_using_saved_graph')
			return 'Showing the last saved graph. Open/edit this template in the editor so the live scope can be computed.';
		// diagnostic === 'ok' (or unknown): genuinely no upstream fields.
		return 'No upstream fields reach this step yet. Connect it after a Start/step that emits fields (or use token["x"] for dynamic data).';
	});

	// The runner auto-imports the SDK and injects these into step scope
	// (executor PythonBackend runner template). Everything else is reachable
	// via `import aithericon`.
	const SDK_HELPERS: { sig: string; doc: string }[] = [
		{
			sig: 'token',
			doc: 'The accumulating workflow token — ready to use, no import. Read fields as token.<name> (see below).'
		},
		{
			sig: 'set_output(name, value)',
			doc: "Emit one field on this node's output port. Downstream steps read it off the token."
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
					Token fields <span class="font-normal text-muted-foreground">— read via <code class="font-mono">token.&lt;name&gt;</code></span>
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
			{#if fields.length > 0}
				<ul class="space-y-0.5">
					{#each fields as f (f.name)}
						<li class="flex items-baseline justify-between gap-2 text-sm">
							<code class="font-mono text-foreground">token.{f.name}</code>
							<span class="shrink-0 text-sm text-muted-foreground">{f.kind}</span>
						</li>
					{/each}
				</ul>
			{:else}
				<p class="text-sm text-muted-foreground">{emptyReason}</p>
				<p class="text-sm text-muted-foreground/70">
					Dynamic access always works: <code class="font-mono">token["field"]</code>.
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
