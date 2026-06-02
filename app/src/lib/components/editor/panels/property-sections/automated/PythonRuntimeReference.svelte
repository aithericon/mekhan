<script lang="ts">
	// Python runtime reference — collapsible help for what the runner injects
	// into the step's global scope. Strictly documentation; the actual list
	// of upstream refs lives in the universal InScopeRefsSection at the top
	// of the rail. Rendered only when the AutomatedStep's backend is python.
	//
	// Canonical access: direct attribute on the upstream slug
	//   (`a = review.invoice_amount`). The runner promotes each upstream
	//   `<slug>` to a Python global via AccessibleDict, so no `token[...]`
	//   wrapping or SDK init is needed in user code.

	type Helper = { sig: string; doc: string };

	const HELPERS: Helper[] = [
		{ sig: 'set_output(name, value)', doc: "Emit one field on this node's output port. Downstream steps borrow it as <slug>.<name>." },
		{ sig: 'log_info(msg, **fields)', doc: 'Structured info log. Extra kwargs become log fields.' },
		{ sig: 'log_warn(msg, **fields)', doc: 'Structured warning log.' },
		{ sig: 'log_error(msg, **fields)', doc: 'Structured error log.' },
		{ sig: 'log_debug(msg, **fields)', doc: 'Structured debug log.' },
		{ sig: 'update_progress(fraction, message=…)', doc: 'Report 0.0–1.0 progress.' },
		{ sig: 'define_phases([…])', doc: 'Declare named phases up front.' },
		{ sig: 'update_phase(name, status)', doc: 'Move a declared phase (e.g. "running", "done").' },
		{ sig: 'log_metric(name, value)', doc: 'Record a single numeric metric.' },
		{ sig: 'log_artifact(path, name=…)', doc: 'Attach a file produced by this step.' },
		{ sig: 'file(path)', doc: 'Wrap an asset File-field storage path as a File. An asset record’s File field is already a File; call .retrieve() (→ local path), .read_bytes() or .read_text() to fetch its bytes on demand.' }
	];
</script>

<details class="group rounded-md border border-border/60 bg-muted/10">
	<summary
		class="flex list-none cursor-pointer select-none items-center justify-between px-2.5 py-1.5 text-sm font-medium text-muted-foreground hover:text-foreground [&::-webkit-details-marker]:hidden"
	>
		<span>Python runtime reference</span>
		<span class="text-muted-foreground transition-transform group-open:rotate-90">›</span>
	</summary>

	<div class="space-y-3 px-2.5 pb-2.5 pt-1">
		<div class="space-y-1">
			<p class="text-sm text-foreground">
				Read upstream fields by their qualified name:
				<code class="font-mono">a = review.invoice_amount</code>.
				Each upstream <code class="font-mono">&lt;slug&gt;</code> is a Python
				global; <code class="font-mono">input.&lt;path&gt;</code> reads from
				the control token.
			</p>
			<p class="text-sm text-muted-foreground">
				Dynamic fallback: <code class="font-mono">token["field"]</code> or
				<code class="font-mono">load_inputs()</code> when the field name isn't
				known statically.
			</p>
		</div>

		<div class="space-y-1">
			<div class="text-sm font-medium text-foreground">
				Curated globals <span class="font-normal text-muted-foreground">— resources &amp; assets by name</span>
			</div>
			<p class="text-sm text-muted-foreground">
				Reference a workspace resource’s field
				(<code class="font-mono">pg.host</code>) or a template asset
				(<code class="font-mono">steel_spec.yield_strength</code>, or the whole
				<code class="font-mono">metals_db</code> collection) as a plain Python
				global — same as an upstream <code class="font-mono">&lt;slug&gt;</code>,
				no binding needed.
			</p>
			<p class="text-sm text-muted-foreground">
				An asset record’s <code class="font-mono">File</code> field is an
				<code class="font-mono">aithericon.File</code>:
				<code class="font-mono">record.datasheet.retrieve()</code> downloads it
				on demand (only the row you pick), returning a local path.
			</p>
		</div>

		<div class="space-y-1.5">
			<div class="text-sm font-medium text-foreground">
				SDK helpers <span class="font-normal text-muted-foreground">— injected, no import</span>
			</div>
			<ul class="space-y-1.5">
				{#each HELPERS as h (h.sig)}
					<li class="text-sm">
						<code class="font-mono text-foreground">{h.sig}</code>
						<p class="mt-0.5 text-sm leading-snug text-muted-foreground">{h.doc}</p>
					</li>
				{/each}
			</ul>
		</div>

		<p class="text-sm leading-snug text-muted-foreground">
			Don't call <code class="font-mono">aithericon.init()</code> /
			<code class="font-mono">shutdown()</code> — the runner manages the IPC
			lifecycle.
		</p>
	</div>
</details>
