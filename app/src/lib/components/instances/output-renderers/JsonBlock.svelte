<script lang="ts">
	import type { RendererProps } from './types';

	let { value }: RendererProps = $props();

	function pretty(v: unknown): string {
		try {
			return JSON.stringify(v, null, 2);
		} catch {
			return String(v);
		}
	}
</script>

<pre class="rounded-md border border-border bg-muted/30 p-3 text-sm font-mono whitespace-pre-wrap break-words">{pretty(value)}</pre>
