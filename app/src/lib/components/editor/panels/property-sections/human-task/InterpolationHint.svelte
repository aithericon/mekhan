<script lang="ts">
	type Props = {
		/** Example token path shown inside the {{ }} braces. */
		example?: string;
	};

	let { example = 'invoice_file.url' }: Props = $props();
</script>

<p class="text-[11px] leading-snug text-muted-foreground">
	Supports <code class="rounded bg-muted px-1 py-0.5 font-mono">{'{{ path }}'}</code>
	interpolation against the Start input token — e.g.
	<code class="rounded bg-muted px-1 py-0.5 font-mono">{`{{ ${example} }}`}</code>
	resolves at instance start.
</p>
