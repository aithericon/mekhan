<script lang="ts">
	// Per-step view of the workflow's resource alias → path bindings.
	//
	// The workflow declares `resources: { alias: type }` at the graph level
	// (B.6); the launcher binds each alias to a concrete resource path at
	// instance-launch time (B.7's `CreateInstanceRequest.resource_bindings`).
	// This panel surfaces:
	//
	//   1. The aliases the workflow declared (read-only here — they're
	//      authored at the workflow level, not per-step).
	//   2. A `ResourcePicker` per alias bound to its declared type, so the
	//      author can pre-select a binding to use at launch.
	//
	// **Where `resources` comes from**: the workflow's `WorkflowGraph.resources`
	// map. The editor's `YjsGraphBinding` doesn't currently materialize that
	// field — graph-binding evolution is the user's parallel work — so this
	// component accepts the map as a prop. The parent passes `graph.resources`
	// when it's threading the full graph, or `{}` when bindings UX is out
	// of scope for that render context.
	//
	// **Where bindings persist**: `bindings` is passed in and changes flow
	// through `onbindings`. v1 keeps these in the Yjs graph's `metadata` (a
	// catch-all map already used for non-typed fields) — exact persistence
	// is wired by the parent. The component itself is stateless.
	import { ResourcePicker } from '$lib/components/resources';

	type Props = {
		/** `alias -> resource_type` from the workflow's top-level
		 *  `resources:` block. Empty when the workflow declares none. */
		resources?: Record<string, string>;
		/** Current `alias -> resource_path` bindings (what the launcher will
		 *  receive). May be partial — unbound aliases render with an empty
		 *  picker. */
		bindings: Record<string, string>;
		readonly?: boolean;
		workspace_id?: string;
		onbindings: (bindings: Record<string, string>) => void;
	};

	let { resources = {}, bindings, readonly = false, workspace_id, onbindings }: Props = $props();

	// Stable, alphabetized presentation matches the backend's `BTreeMap`
	// serialization order — so the picker order is the same as the wire
	// order is the same as the YAML order an author writes.
	const aliases = $derived(Object.keys(resources).sort());

	function setBinding(alias: string, path: string | null) {
		const next = { ...bindings };
		if (path === null || path === '') {
			delete next[alias];
		} else {
			next[alias] = path;
		}
		onbindings(next);
	}
</script>

{#if aliases.length > 0}
	<details
		class="group rounded-md border border-border/60 bg-muted/10"
		open
		data-testid="resource-bindings"
	>
		<summary
			class="flex list-none cursor-pointer select-none items-center justify-between px-2.5 py-1.5 text-sm font-medium text-muted-foreground hover:text-foreground [&::-webkit-details-marker]:hidden"
		>
			<span>Resource bindings</span>
			<span class="text-muted-foreground transition-transform group-open:rotate-90">›</span>
		</summary>
		<div class="space-y-3 px-2.5 pb-2.5 pt-1">
			<p class="text-sm text-muted-foreground">
				Each declared alias resolves to a concrete resource at launch.
				The picker filters by the alias's declared type.
			</p>
			{#each aliases as alias (alias)}
				<div class="space-y-1.5">
					<div class="flex items-baseline gap-2">
						<span class="font-mono text-sm text-foreground">{alias}</span>
						<span class="text-sm text-muted-foreground">{resources[alias]}</span>
					</div>
					<ResourcePicker
						type={resources[alias]}
						value={bindings[alias] ?? null}
						{workspace_id}
						disabled={readonly}
						placeholder={`Pick a ${resources[alias]} resource…`}
						onchange={(path) => setBinding(alias, path)}
					/>
				</div>
			{/each}
		</div>
	</details>
{/if}
