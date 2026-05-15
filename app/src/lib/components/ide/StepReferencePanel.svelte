<script lang="ts">
	// Read-only authoring reference for a Python automated step: the token
	// fields readable here (this node's input scope, the same `input.<field>`
	// set the generated `.pyi` types) plus the SDK helpers the runner injects.
	// Purely presentational — data comes from `getStepScopes()`.
	import type { StepScopeField } from '$lib/api/client';

	type Props = {
		/** This node's input scope: fields readable as `token.<name>`. */
		fields: StepScopeField[];
	};

	let { fields }: Props = $props();

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
		class="flex list-none cursor-pointer select-none items-center justify-between px-3 py-2 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground hover:text-foreground [&::-webkit-details-marker]:hidden"
	>
		<span>Reference</span>
		<span class="text-muted-foreground transition-transform group-open:rotate-90">›</span>
	</summary>

	<div class="max-h-[45vh] space-y-4 overflow-y-auto px-3 pb-3">
		<div class="space-y-1.5">
			<div class="text-xs font-medium text-foreground">
				Token fields <span class="font-normal text-muted-foreground">— read via <code class="font-mono">token.&lt;name&gt;</code></span>
			</div>
			{#if fields.length > 0}
				<ul class="space-y-0.5">
					{#each fields as f (f.name)}
						<li class="flex items-baseline justify-between gap-2 text-xs">
							<code class="font-mono text-foreground">token.{f.name}</code>
							<span class="shrink-0 text-[10px] text-muted-foreground">{f.kind}</span>
						</li>
					{/each}
				</ul>
			{:else}
				<p class="text-xs text-muted-foreground">
					No typed upstream fields — this step's token is pass-through. Use
					<code class="font-mono">token["field"]</code> for dynamic access.
				</p>
			{/if}
		</div>

		<div class="space-y-1.5">
			<div class="text-xs font-medium text-foreground">
				SDK helpers <span class="font-normal text-muted-foreground">— injected, no import</span>
			</div>
			<ul class="space-y-1.5">
				{#each SDK_HELPERS as h (h.sig)}
					<li class="text-xs">
						<code class="font-mono text-foreground">{h.sig}</code>
						<p class="mt-0.5 text-[11px] leading-snug text-muted-foreground">{h.doc}</p>
					</li>
				{/each}
			</ul>
		</div>

		<p class="text-[11px] leading-snug text-muted-foreground">
			Don't call <code class="font-mono">aithericon.init()</code> / <code class="font-mono">shutdown()</code> —
			the runner manages the IPC lifecycle. Fields update on publish from the live graph.
		</p>
	</div>
</details>
