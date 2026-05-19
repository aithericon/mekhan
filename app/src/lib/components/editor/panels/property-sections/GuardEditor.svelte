<script lang="ts">
	// Visual editor for a single Decision/Loop guard. Two modes:
	//   • "simple": a single LHS-op-RHS row builder that produces Rhai like
	//     `start.amount > 1000`. Covers ~80% of real-world guards and gives
	//     non-developer authors a discoverable surface.
	//   • "advanced": raw Rhai textbox (the existing CodeEditor), for guards
	//     the row builder can't express (e.g. `start.x && start.y`).
	//
	// The simple form auto-collapses to advanced when the persisted guard
	// doesn't parse back into a single comparison row — round-trippable on
	// initial mount, sticky after the user toggles.

	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import CodeEditor from '../shared/CodeEditor.svelte';
	import RefPicker from './RefPicker.svelte';
	import { Input } from '$lib/components/ui/input';
	import * as Select from '$lib/components/ui/select';
	import Code from '@lucide/svelte/icons/code';
	import Wrench from '@lucide/svelte/icons/wrench';
	import type { components } from '$lib/api/schema';

	type FieldKind = components['schemas']['FieldKind'];

	type Props = {
		guard: string;
		scope: ScopeEntry[];
		readonly?: boolean;
		onchange: (guard: string) => void;
	};

	let { guard, scope, readonly = false, onchange }: Props = $props();

	// Possible operators. Restricted to a Rhai-safe subset so the simple
	// builder never round-trips broken syntax.
	const operators = [
		{ value: '==', label: '=' },
		{ value: '!=', label: '≠' },
		{ value: '>', label: '>' },
		{ value: '>=', label: '≥' },
		{ value: '<', label: '<' },
		{ value: '<=', label: '≤' }
	] as const;

	type Parsed = { lhs: string; op: string; rhs: string } | null;

	// Try to parse the current guard string into a single LHS-op-RHS row.
	// Returns null if the guard is empty, contains boolean combinators, or
	// can't be cleanly decomposed.
	function tryParse(g: string): Parsed {
		const trimmed = g.trim();
		if (!trimmed) return null;
		// Reject anything containing logical combinators / function calls /
		// blocks — those can't survive the row form.
		if (/&&|\|\||\(|\)|;|\{|\}|!|\bif\b|\blet\b/.test(trimmed)) return null;

		// Match `<qualified> <op> <rest>`. The qualified LHS is `ident.ident`
		// optionally with whitespace.
		const re = /^([A-Za-z_][A-Za-z0-9_]*\s*\.\s*[A-Za-z_][A-Za-z0-9_]*)\s*(==|!=|>=|<=|>|<)\s*(.+?)\s*$/;
		const m = trimmed.match(re);
		if (!m) return null;
		return {
			lhs: m[1].replace(/\s+/g, ''),
			op: m[2],
			rhs: m[3]
		};
	}

	let parsed = $derived(tryParse(guard));

	// "advanced" mode tracks whether the user explicitly toggled into raw
	// Rhai. Once raw, stay raw — the builder shouldn't recapture authored
	// expressions and lose meaning.
	let sticky_advanced = $state(false);
	let advanced = $derived(sticky_advanced || (guard.trim().length > 0 && parsed === null));

	// Bool fields don't need an RHS picker — they read as just the
	// identifier ("approved"). When the user selects a Bool field and no
	// RHS, default to `== true`.
	function fieldKind(qualified: string): FieldKind | null {
		const entry = scope.find((s) => s.qualified === qualified);
		return entry?.kind ?? null;
	}

	function emit(lhs: string, op: string, rhs: string) {
		const trimmedLhs = lhs.trim();
		const trimmedRhs = rhs.trim();
		if (!trimmedLhs) {
			onchange('');
			return;
		}
		if (!trimmedRhs) {
			// Default: bool fields get `== true`, others stay incomplete (no
			// emit) so the user sees the empty input rather than a broken
			// guard.
			if (fieldKind(trimmedLhs) === 'bool') {
				onchange(`${trimmedLhs} == true`);
			} else {
				onchange(`${trimmedLhs} ${op}`.trim());
			}
			return;
		}
		onchange(`${trimmedLhs} ${op} ${trimmedRhs}`);
	}

	let pendingOp = $state('==');
	$effect(() => {
		if (parsed) pendingOp = parsed.op;
	});

	function setLhs(value: string) {
		// Round-trip through `parsed` so we don't clobber an existing rhs.
		const cur = parsed ?? { lhs: '', op: pendingOp, rhs: '' };
		emit(value, cur.op, cur.rhs);
	}

	function setOp(value: string) {
		pendingOp = value;
		const cur = parsed ?? { lhs: '', op: value, rhs: '' };
		emit(cur.lhs, value, cur.rhs);
	}

	function setRhs(value: string) {
		const cur = parsed ?? { lhs: '', op: pendingOp, rhs: '' };
		emit(cur.lhs, cur.op, value);
	}
</script>

<div class="space-y-1.5">
	<div class="flex items-center justify-between">
		<span class="text-sm text-muted-foreground">Guard</span>
		<button
			type="button"
			class="flex items-center gap-1.5 rounded px-2 py-1 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
			onclick={() => (sticky_advanced = !advanced)}
			disabled={readonly}
			title={advanced ? 'Switch to simple builder' : 'Switch to raw Rhai'}
		>
			{#if advanced}
				<Wrench class="size-4" />
				Builder
			{:else}
				<Code class="size-4" />
				Rhai
			{/if}
		</button>
	</div>

	{#if advanced}
		<CodeEditor
			value={guard}
			language="rhai"
			{readonly}
			minHeight="40px"
			maxHeight="100px"
			onchange={(val) => onchange(val)}
		/>
		{#if scope.length > 0}
			<div class="pt-1">
				<RefPicker
					{scope}
					disabled={readonly}
					placeholder="Insert reference…"
					onpick={(e) => onchange((guard ? guard + ' ' : '') + e.qualified)}
				/>
			</div>
		{:else}
			<div class="pt-1 text-sm text-muted-foreground italic">
				No upstream fields in scope. Wire a Start or AutomatedStep upstream and declare its
				output port to reference fields here.
			</div>
		{/if}
	{:else}
		<div class="flex items-center gap-2">
			<!-- LHS: qualified field picker (two-column node → variable) -->
			<div class="min-w-0 flex-1">
				<RefPicker
					{scope}
					disabled={readonly || scope.length === 0}
					selected={parsed?.lhs}
					placeholder="Pick field…"
					onpick={(e) => setLhs(e.qualified)}
				/>
			</div>

			<!-- Operator -->
			<Select.Root
				type="single"
				value={parsed?.op ?? pendingOp}
				onValueChange={(v) => v && setOp(v)}
				disabled={readonly}
			>
				<Select.Trigger class="h-9 w-16 px-2 text-sm">
					<span class="font-mono">{operators.find((o) => o.value === (parsed?.op ?? pendingOp))?.label ?? '='}</span>
				</Select.Trigger>
				<Select.Content>
					{#each operators as op (op.value)}
						<Select.Item value={op.value} label={op.label}>
							<span class="font-mono">{op.label}</span>
							<span class="ml-2 text-sm text-muted-foreground">{op.value}</span>
						</Select.Item>
					{/each}
				</Select.Content>
			</Select.Root>

			<!-- RHS: free-text Rhai literal (string with quotes, number, true/false) -->
			<Input
				type="text"
				value={parsed?.rhs ?? ''}
				placeholder={parsed && fieldKind(parsed.lhs) === 'bool' ? 'true' : 'value'}
				disabled={readonly}
				oninput={(e) => setRhs((e.currentTarget as HTMLInputElement).value)}
				class="h-9 flex-1 px-3 text-sm font-mono"
			/>
		</div>
		{#if guard.trim().length > 0}
			<div class="font-mono text-sm text-muted-foreground">
				{guard}
			</div>
		{/if}
	{/if}
</div>
