<!--
  ArtifactProvenance — a compact provenance line for one catalogue artifact:
  which step produced it, its category/type/size, when, and any producer-declared
  parameters (user_metadata, minus the render_hint plumbing key). Used by embedded
  Report media blocks and the live artifact panels so a chart in a report carries
  its origin, not just a filename.
-->
<script lang="ts">
	import type { LiveArtifactEntry } from '$lib/api/client';
	import { Badge } from '$lib/components/ui/badge';
	import { cn } from '$lib/utils';

	let { entry, class: className = '' }: { entry: LiveArtifactEntry; class?: string } = $props();

	function shortType(mime: string | null | undefined): string | null {
		if (!mime) return null;
		const sub = mime.split('/')[1] ?? mime;
		return (sub.split(';')[0] || mime).toUpperCase();
	}
	function fmtBytes(b: number | null | undefined): string | null {
		if (b == null) return null;
		if (b < 1024) return `${b} B`;
		if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} KB`;
		return `${(b / (1024 * 1024)).toFixed(1)} MB`;
	}
	function fmtDate(s: string | null | undefined): string | null {
		if (!s) return null;
		const d = new Date(s);
		return isNaN(d.getTime())
			? null
			: d.toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' });
	}

	// Producer-declared parameters: user_metadata minus the render_hint plumbing
	// key, primitives only (the firing curve's ramp/hold/cool, a seed, etc.).
	const params = $derived.by<[string, string][]>(() => {
		const m = entry.user_metadata ?? {};
		const out: [string, string][] = [];
		for (const [k, v] of Object.entries(m)) {
			if (k === 'render_hint') continue;
			if (v == null || typeof v === 'object') continue;
			out.push([k, String(v)]);
		}
		return out;
	});

	const facts = $derived.by<string[]>(() => {
		const f: string[] = [];
		if (entry.process_step) f.push(`Step ${entry.process_step}`);
		if (entry.category) f.push(entry.category);
		const t = shortType(entry.mime_type);
		if (t) f.push(t);
		const sz = fmtBytes(entry.size_bytes);
		if (sz) f.push(sz);
		const dt = fmtDate(entry.created_at);
		if (dt) f.push(dt);
		return f;
	});
</script>

{#if facts.length || params.length}
	<div class={cn('flex flex-col gap-1', className)}>
		{#if facts.length}
			<div class="flex flex-wrap items-center gap-x-1.5 text-xs text-muted-foreground">
				{#each facts as f, i (i)}
					{#if i > 0}<span class="opacity-40">·</span>{/if}
					<span>{f}</span>
				{/each}
			</div>
		{/if}
		{#if params.length}
			<div class="flex flex-wrap gap-1">
				{#each params as [k, v] (k)}
					<Badge variant="outline" class="font-mono text-[10px] font-normal">{k}: {v}</Badge>
				{/each}
			</div>
		{/if}
	</div>
{/if}
