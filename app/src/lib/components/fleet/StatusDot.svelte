<script lang="ts">
	// A single liveness dot, shared by every Fleet view (runner stations, worker
	// cards, backend-coverage tiles, list rows). One place owns the tone→colour map.
	//   live → emerald (present / covered)
	//   idle → muted   (offline / absent)
	//   warn → amber   (uncovered / unbacked)
	let { tone = 'idle', class: klass = '' }: { tone?: 'live' | 'idle' | 'warn'; class?: string } =
		$props();

	const colour = $derived(
		tone === 'live' ? 'bg-emerald-500' : tone === 'warn' ? 'bg-amber-500' : 'bg-muted-foreground/30'
	);
</script>

<span class="inline-block size-2.5 shrink-0 rounded-full {colour} {klass}"></span>
