<script lang="ts" module>
	/**
	 * Exported types used by the Integrate stage.
	 *
	 * SchemaBuilder is the primary deliverable of the builder stage. It is a thin
	 * root wrapper around BuilderNodeEditor (the recursive body): it owns the
	 * depth-0 root-kind switcher and the raw-JSON escape hatch, parses the
	 * incoming JSON Schema into a BuilderNode once, and serialises edits back out.
	 *
	 * The component works with JSON Schema as its stored form (the `schema` prop);
	 * BuilderNode is an internal edit model, not exposed via props.
	 */
	export type { BuilderNode, BuilderField, FieldKindHint } from './builder-model';
</script>

<script lang="ts">
	import { untrack } from 'svelte';
	import AlertCircle from '@lucide/svelte/icons/alert-circle';
	import Code from '@lucide/svelte/icons/code';
	import BuilderNodeEditor from './BuilderNodeEditor.svelte';
	import {
		schemaToBuilderNode,
		builderNodeToSchema,
		type BuilderNode
	} from './builder-model';

	// ── Props ────────────────────────────────────────────────────────────────

	type Props = {
		schema: unknown;
		onchange: (schema: unknown) => void;
		/**
		 * When true the root node may be a scalar (Agent use-case).
		 * When false (default) only object/array are valid root shapes; the
		 * root-type toggle is restricted accordingly.
		 */
		allowRootScalar?: boolean;
		/**
		 * Workflow-level reusable schema definitions (name → schema). When
		 * provided, a "$ref" type option becomes available in the root-kind
		 * toggle and the union variant adder. Callers should pass
		 * `getWorkflowDefinitions()` from `$lib/editor/workflow-definitions.svelte`.
		 * Pass an empty object (the default) when definitions aren't reachable.
		 */
		definitions?: Record<string, unknown>;
		/** When true, all editing controls are disabled. */
		readonly?: boolean;
	};

	let {
		schema,
		onchange,
		allowRootScalar = false,
		definitions = {},
		readonly = false
	}: Props = $props();

	// ── Parse schema into builder node ───────────────────────────────────────

	// We keep a local editable BuilderNode. Edits from the child mutate it and we
	// emit `builderNodeToSchema(node)` upward. We do NOT re-parse on our own
	// emissions — that would clobber in-progress edits. We re-parse only when an
	// _incoming_ schema prop differs STRUCTURALLY from what we last emitted (the
	// parent replaced the schema, e.g. via the raw-JSON editor or an external
	// reset).

	// Intentional snapshot-only initializer: prop changes are tracked via the
	// $effect below. untrack() suppresses the "state_referenced_locally" warning.
	let node = $state<BuilderNode>(untrack(() => schemaToBuilderNode(schema)));

	// Structural snapshot of the last schema we emitted, for comparison.
	let lastEmitted = $state<string | null>(null);

	$effect(() => {
		// Reading `schema` here makes this effect re-run on prop changes.
		const incoming = schema;
		untrack(() => {
			const incomingStr = JSON.stringify(incoming);
			// Only re-parse when the incoming prop is structurally different from
			// what we last emitted — a structurally-equal re-render must not blow
			// away in-progress edits.
			if (incomingStr !== lastEmitted) {
				node = schemaToBuilderNode(incoming);
			}
		});
	});

	// ── Emit ──────────────────────────────────────────────────────────────────

	function applyNode(next: BuilderNode) {
		node = next;
		const s = builderNodeToSchema(next);
		lastEmitted = JSON.stringify(s);
		onchange(s);
	}

	// ── Raw-JSON escape state ─────────────────────────────────────────────────

	let rawText = $state('');
	let rawError = $state('');
	let rawMode = $state(false);

	function enterRawMode() {
		rawText = JSON.stringify(builderNodeToSchema(node), null, 2);
		rawError = '';
		rawMode = true;
	}

	function exitRawMode() {
		try {
			const parsed = JSON.parse(rawText);
			const next = schemaToBuilderNode(parsed);
			applyNode(next);
			rawMode = false;
		} catch (e) {
			rawError = `Invalid JSON: ${(e as Error).message}`;
		}
	}

	// ── Root type switcher ───────────────────────────────────────────────────

	type RootKind = 'object' | 'array' | 'scalar' | 'union' | 'ref';

	const rootKind = $derived<RootKind>(
		node.kind === 'array'
			? 'array'
			: node.kind === 'scalar'
				? 'scalar'
				: node.kind === 'union'
					? 'union'
					: node.kind === 'ref'
						? 'ref'
						: 'object'
	);

	const definitionNames = $derived(Object.keys(definitions));

	function setRootKind(k: RootKind) {
		if (k === rootKind) return;
		if (k === 'object') {
			applyNode({ kind: 'object', fields: [], required: new Set(), sealed: false, nullable: false });
		} else if (k === 'array') {
			applyNode({
				kind: 'array',
				items: { kind: 'object', fields: [], required: new Set(), sealed: false, nullable: false },
				nullable: false
			});
		} else if (k === 'scalar') {
			applyNode({ kind: 'scalar', type: 'string', nullable: false, enumValues: [] });
		} else if (k === 'union') {
			applyNode({
				kind: 'union',
				combinator: 'oneOf',
				variants: [
					{ kind: 'object', fields: [], required: new Set(), sealed: false, nullable: false },
					{ kind: 'object', fields: [], required: new Set(), sealed: false, nullable: false }
				]
			});
		} else if (k === 'ref') {
			// Pick first available definition or blank name.
			const firstName = definitionNames[0] ?? '';
			applyNode({ kind: 'ref', name: firstName });
		}
	}
</script>

<div class="min-w-0 space-y-2" data-testid="schema-builder" data-depth={0}>
	{#if rawMode}
		<!-- ── Raw JSON editor ─────────────────────────────────────────────── -->
		<div class="space-y-2">
			<div class="flex items-center justify-between">
				<span class="text-xs font-medium text-muted-foreground">Raw JSON Schema</span>
				<button
					type="button"
					class="text-xs text-primary underline-offset-2 hover:underline"
					onclick={exitRawMode}
				>
					Apply &amp; return to builder
				</button>
			</div>
			<textarea
				class="font-mono text-xs w-full min-h-[140px] rounded-md border border-border bg-background px-3 py-2 text-sm shadow-xs outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50 field-sizing-content"
				bind:value={rawText}
				spellcheck="false"
				data-testid="schema-builder-raw-textarea"
			></textarea>
			{#if rawError}
				<p class="flex items-center gap-1.5 text-xs text-destructive" role="alert">
					<AlertCircle class="size-3 shrink-0" />
					{rawError}
				</p>
			{/if}
		</div>
	{:else}
		<!-- ── Root type selector ─────────────────────────────────────────── -->
		<div class="flex flex-wrap gap-1.5" data-testid="schema-builder-root-kind">
			<button
				type="button"
				class="flex-1 rounded-md border px-2 py-1 text-sm transition-colors {rootKind === 'object'
					? 'border-primary bg-primary/5 text-foreground'
					: 'border-border text-muted-foreground hover:bg-accent/30'}"
				disabled={readonly}
				onclick={() => setRootKind('object')}
				data-testid="schema-builder-kind-object"
			>
				Object
			</button>
			<button
				type="button"
				class="flex-1 rounded-md border px-2 py-1 text-sm transition-colors {rootKind === 'array'
					? 'border-primary bg-primary/5 text-foreground'
					: 'border-border text-muted-foreground hover:bg-accent/30'}"
				disabled={readonly}
				onclick={() => setRootKind('array')}
				data-testid="schema-builder-kind-array"
			>
				Array
			</button>
			{#if allowRootScalar}
				<button
					type="button"
					class="flex-1 rounded-md border px-2 py-1 text-sm transition-colors {rootKind === 'scalar'
						? 'border-primary bg-primary/5 text-foreground'
						: 'border-border text-muted-foreground hover:bg-accent/30'}"
					disabled={readonly}
					onclick={() => setRootKind('scalar')}
					data-testid="schema-builder-kind-scalar"
				>
					Scalar
				</button>
			{/if}
			<button
				type="button"
				class="flex-1 rounded-md border px-2 py-1 text-sm transition-colors {rootKind === 'union'
					? 'border-primary bg-primary/5 text-foreground'
					: 'border-border text-muted-foreground hover:bg-accent/30'}"
				disabled={readonly}
				onclick={() => setRootKind('union')}
				data-testid="schema-builder-kind-union"
				title="oneOf / anyOf union"
			>
				Union
			</button>
			{#if definitionNames.length > 0}
				<button
					type="button"
					class="flex-1 rounded-md border px-2 py-1 text-sm transition-colors {rootKind === 'ref'
						? 'border-primary bg-primary/5 text-foreground'
						: 'border-border text-muted-foreground hover:bg-accent/30'}"
					disabled={readonly}
					onclick={() => setRootKind('ref')}
					data-testid="schema-builder-kind-ref"
					title="Reference a named definition"
				>
					$ref
				</button>
			{/if}
			<button
				type="button"
				class="rounded-md border border-border px-2 py-1 text-muted-foreground transition-colors hover:bg-accent/30"
				title="Edit raw JSON Schema"
				disabled={readonly}
				onclick={enterRawMode}
				data-testid="schema-builder-raw-toggle"
			>
				<Code class="size-4" />
			</button>
		</div>

		<!-- ── Recursive editor body ──────────────────────────────────────── -->
		<BuilderNodeEditor
			{node}
			onNodeChange={applyNode}
			{allowRootScalar}
			{definitions}
			{readonly}
			_depth={0}
			onEditRaw={enterRawMode}
		/>
	{/if}
</div>
