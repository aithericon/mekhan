<script lang="ts">
	import type { DecisionNodeData } from '$lib/types/editor';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import Plus from '@lucide/svelte/icons/plus';
	import GuardEditor from './GuardEditor.svelte';
	import { Input } from '$lib/components/ui/input';

	type Props = {
		data: DecisionNodeData;
		readonly?: boolean;
		onchange: (data: DecisionNodeData) => void;
		/**
		 * In-scope identifiers for guards on this node, precomputed by the
		 * parent panel via `computeScopes(graph).get(nodeId)`. Empty if the
		 * node is detached or has no typed-port upstreams.
		 */
		scope?: ScopeEntry[];
	};

	let { data, readonly = false, onchange, scope = [] }: Props = $props();

	function addBranch() {
		onchange({
			...data,
			conditions: [
				...data.conditions,
				{
					edgeId: `branch-${Date.now()}`,
					label: `Branch ${data.conditions.length + 1}`,
					guard: ''
				}
			]
		});
	}

	function updateConditionLabel(index: number, label: string) {
		const updated = [...data.conditions];
		updated[index] = { ...updated[index], label };
		onchange({ ...data, conditions: updated });
	}

	function updateConditionGuard(index: number, guard: string) {
		const updated = [...data.conditions];
		updated[index] = { ...updated[index], guard };
		onchange({ ...data, conditions: updated });
	}

	// The default (else) branch handle id is the literal "default" — it must
	// match DecisionNode's `<Handle id="default">` and the compiler's default
	// output place so a drawn edge wires.
	const DEFAULT_BRANCH_ID = 'default';
	function toggleDefault(enabled: boolean) {
		onchange({ ...data, defaultBranch: enabled ? DEFAULT_BRANCH_ID : undefined });
	}
</script>

<div class="space-y-2">
	<div class="flex items-center justify-between">
		<span class="text-xs font-medium text-muted-foreground">Branches</span>
		{#if !readonly}
			<button
				type="button"
				class="flex items-center gap-1 rounded-md px-2 py-0.5 text-[10px] font-medium text-primary transition-colors hover:bg-accent"
				onclick={addBranch}
			>
				<Plus class="size-3" />
				Add Branch
			</button>
		{/if}
	</div>

	{#each data.conditions as condition, i (condition.edgeId)}
		<div class="rounded-lg border border-border bg-muted/30 p-2 text-[11px]">
			<div class="space-y-1.5">
				<Input
					type="text"
					value={condition.label}
					placeholder="Branch label"
					disabled={readonly}
					oninput={(e) => updateConditionLabel(i, (e.currentTarget as HTMLInputElement).value)}
					class="h-7 px-2 py-1 text-[11px]"
				/>
				<GuardEditor
					guard={condition.guard}
					{scope}
					{readonly}
					onchange={(val) => updateConditionGuard(i, val)}
				/>
			</div>
		</div>
	{/each}

	<label
		class="flex items-center gap-2 rounded-lg border border-dashed border-border p-2 text-[11px] text-muted-foreground"
	>
		<input
			type="checkbox"
			checked={!!data.defaultBranch}
			disabled={readonly}
			data-testid="checkbox-default-branch"
			onchange={(e) => toggleDefault((e.currentTarget as HTMLInputElement).checked)}
		/>
		<span>
			Add default (else) branch — taken when no guard matches. Wire its
			handle to a fallback path.
		</span>
	</label>
</div>
