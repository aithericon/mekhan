<script lang="ts">
	import type { DecisionNodeData } from '$lib/types/editor';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import ChevronUp from '@lucide/svelte/icons/chevron-up';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import Info from '@lucide/svelte/icons/info';
	import GuardEditor from './GuardEditor.svelte';
	import { Input } from '$lib/components/ui/input';
	import { Button } from '$lib/components/ui/button';
	import * as Tooltip from '$lib/components/ui/tooltip';

	type Props = {
		data: DecisionNodeData;
		readonly?: boolean;
		onchange: (data: DecisionNodeData) => void;
		/**
		 * In-scope identifiers for guards on this node, fetched by the parent
		 * panel from the backend shape-aware analyzer (`POST /api/analyze`,
		 * the single source of truth). Empty if detached or unresolvable.
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

	function removeBranch(index: number) {
		onchange({
			...data,
			conditions: data.conditions.filter((_, i) => i !== index)
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

	// Branch order IS precedence: the compiler lowers branches as a
	// switch/case cascade (branch i fires only when its guard holds and no
	// earlier branch matched), so reordering changes which branch wins when
	// guards overlap. Edge wiring is keyed by the stable edgeId, so a reorder
	// never drops a drawn edge.
	function moveBranch(index: number, direction: -1 | 1) {
		const target = index + direction;
		if (target < 0 || target >= data.conditions.length) return;
		const updated = [...data.conditions];
		[updated[index], updated[target]] = [updated[target], updated[index]];
		onchange({ ...data, conditions: updated });
	}

	// The default (else) branch handle id is the literal "default" — it must
	// match DecisionNode's `<Handle id="default">` and the compiler's default
	// output place so a drawn edge wires. Mirrors the Rust constant
	// `DEFAULT_BRANCH_HANDLE_ID` in service/src/models/template.rs; the
	// compiler's `validate` pass rejects any other value.
	const DEFAULT_BRANCH_ID = 'default';
	function toggleDefault(enabled: boolean) {
		onchange({ ...data, defaultBranch: enabled ? DEFAULT_BRANCH_ID : undefined });
	}
</script>

<div class="space-y-2">
	<div class="flex items-center justify-between">
		<div class="flex items-center gap-1.5">
			<span class="text-sm font-medium text-muted-foreground">Branches</span>
			<Tooltip.Provider delayDuration={150}>
				<Tooltip.Root>
					<Tooltip.Trigger
						class="text-muted-foreground transition-colors hover:text-foreground"
						aria-label="How branch ordering works"
					>
						<Info class="size-4" />
					</Tooltip.Trigger>
					<Tooltip.Content side="bottom" class="max-w-xs text-sm leading-snug">
						Order is precedence: branches are evaluated top-to-bottom and the
						first matching guard wins. The default (else) branch is always
						evaluated last.
					</Tooltip.Content>
				</Tooltip.Root>
			</Tooltip.Provider>
		</div>
		{#if !readonly}
			<button
				type="button"
				class="flex items-center gap-1 rounded-md px-2 py-0.5 text-sm font-medium text-primary transition-colors hover:bg-accent"
				onclick={addBranch}
			>
				<Plus class="size-3" />
				Add Branch
			</button>
		{/if}
	</div>

	{#each data.conditions as condition, i (condition.edgeId)}
		<div class="rounded-lg border border-border bg-muted/30 p-2 text-sm">
			<div class="space-y-1.5">
				<div class="flex items-center gap-2">
					<span
						class="flex size-4 shrink-0 items-center justify-center rounded-sm bg-muted text-sm font-semibold text-muted-foreground"
						title="Precedence"
					>
						{i + 1}
					</span>
					<Input
						type="text"
						value={condition.label}
						placeholder="Branch label"
						disabled={readonly}
						oninput={(e) => updateConditionLabel(i, (e.currentTarget as HTMLInputElement).value)}
						class="h-7 px-2 py-1 text-sm"
					/>
					{#if !readonly}
						<Button
							variant="ghost"
							size="sm"
							disabled={i === 0}
							onclick={() => moveBranch(i, -1)}
							aria-label="Move branch up"
						>
							<ChevronUp class="size-3.5" />
						</Button>
						<Button
							variant="ghost"
							size="sm"
							disabled={i === data.conditions.length - 1}
							onclick={() => moveBranch(i, 1)}
							aria-label="Move branch down"
						>
							<ChevronDown class="size-3.5" />
						</Button>
						<Button
							variant="ghost"
							size="sm"
							onclick={() => removeBranch(i)}
							aria-label="Remove branch"
						>
							<Trash2 class="size-3.5" />
						</Button>
					{/if}
				</div>
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
		class="flex items-center gap-2 rounded-lg border border-dashed border-border p-2 text-sm text-muted-foreground"
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
