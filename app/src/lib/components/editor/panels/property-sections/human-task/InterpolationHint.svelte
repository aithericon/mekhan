<script lang="ts">
	// Shown only when no upstream scope is in reach (no upstream Start /
	// HumanTask / AutomatedStep producer wired in yet, so the picker has
	// nothing to offer). Once scope arrives, the parent renders an
	// `InsertRefButton` instead — the picker is the canonical affordance.
	type Props = {
		/** Example reference shown inside the {{ }} braces. */
		example?: string;
	};

	let { example = 'invoice_file.url' }: Props = $props();
</script>

<p class="text-sm leading-snug text-muted-foreground">
	Wire an upstream Start / HumanTask / AutomatedStep to pick references
	with the variable picker. Supports
	<code class="rounded bg-muted px-1 py-0.5 font-mono">{'{{ <slug>.<field> }}'}</code>
	(producer outputs) and bare
	<code class="rounded bg-muted px-1 py-0.5 font-mono">{'{{ field }}'}</code>
	(Start fields) — e.g.
	<code class="rounded bg-muted px-1 py-0.5 font-mono">{`{{ ${example} }}`}</code>.
	Resolved at the wire-edge before the task is rendered.
</p>
