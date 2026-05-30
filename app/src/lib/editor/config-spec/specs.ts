/**
 * SPIKE — config-spec/specs.ts
 *
 * One NodeConfigSpec per migrated node.  Fields faithfully reproduce
 * what the bespoke section components offered (same data keys, same labels,
 * same widget semantics).
 *
 * This file is PURE DATA — it never imports .svelte files. Custom component
 * references are string keys resolved via custom-registry.ts at runtime.
 */

import type { NodeConfigSpec } from './types';


// ---------------------------------------------------------------------------
// delay
//
// Original: DelayNodeSection.svelte — single Rhai-expression field
// `durationMsExpr` rendered via GuardEditor (CodeEditor language="rhai" +
// RefPicker for ref insertion).  We represent the expression field as
// kind:'code' lang:'rhai' so FieldRenderer uses CodeEditor, which is the same
// underlying widget GuardEditor falls back to in its "advanced" mode.
//
// Note: The simple-builder (LHS/op/RHS row) that GuardEditor also provides is
// not reproduced here — it is specific to Decision/Loop guards.  For a delay
// duration that builder doesn't apply; the raw Rhai expression is the correct
// surface.  A future 'expr' kind could encapsulate the full GuardEditor widget
// if needed.
// ---------------------------------------------------------------------------
export const DELAY_SPEC: NodeConfigSpec = {
	fields: [
		{
			kind: 'code',
			bind: 'durationMsExpr',
			label: 'Wait for (ms)',
			description:
				'Rhai expression returning the delay in milliseconds.  Plain numbers like 5000 work; refs like order.sla_ms resolve against upstream parked data via standard read-arc synthesis.',
			lang: 'rhai',
			minHeight: '40px',
			maxHeight: '100px'
		}
	]
};

// ---------------------------------------------------------------------------
// progress_update
//
// Original: ProgressUpdateNodeSection.svelte.  Fields:
//   fraction      number 0..1  (clamped, shown as pct% in label)
//   currentStep   number optional
//   totalSteps    number optional
//   message       string optional, `{{ ref }}` interpolation via InsertRefButton
//
// The dynamic "Fraction (0–1) — {pct}%" label is handled by FieldRenderer
// when it detects a 'number' spec with transform:'clamp01' — it derives the
// label suffix from the live value.  The currentStep/totalSteps flex-row
// layout is delegated to SchemaDrivenSection (it wraps them in a flex div
// when consecutive fields carry a shared `groupId`).
//
// For the spike we keep all fields flat and accept the minor layout difference
// (currentStep and totalSteps stack vertically rather than sitting side-by-side).
// A 'group' layout hint is a natural follow-up to NodeConfigSpec.
// ---------------------------------------------------------------------------
export const PROGRESS_UPDATE_SPEC: NodeConfigSpec = {
	fields: [
		{
			kind: 'number',
			bind: 'fraction',
			label: 'Fraction (0–1)',
			description: 'Progress fraction from 0.0 to 1.0 (e.g. 0.5 = 50%).',
			min: 0,
			max: 1,
			step: 0.05,
			transform: 'clamp01'
		},
		{
			kind: 'number',
			bind: 'currentStep',
			label: 'Current step (optional)',
			min: 0,
			transform: 'optInt'
		},
		{
			kind: 'number',
			bind: 'totalSteps',
			label: 'Total steps (optional)',
			min: 0,
			transform: 'optInt'
		},
		{
			kind: 'textarea',
			bind: 'message',
			label: 'Message (optional)',
			rows: 2,
			placeholder: 'e.g. Processed {{ count }} rows',
			description:
				'{{ ref }} placeholders interpolate fields from this node\'s input scope — use the picker above for the in-scope set.'
		}
	]
};

// ---------------------------------------------------------------------------
// timeout
//
// Original: TimeoutNodeSection.svelte — single Rhai-expression field
// `durationMsExpr` rendered via GuardEditor (CodeEditor language="rhai" +
// embedded RefPicker).  We represent this as kind:'code' lang:'rhai',
// identical to DELAY_SPEC.durationMsExpr — only label and description differ.
//
// The bespoke section had two prose blocks:
//   (1) intro paragraph above the field → folded into field.description
//   (2) "v1 limitation" paragraph below → appended to field.description
//
// Both are preserved as a single description string; the layout delta
// (note appears above the field rather than below) matches the accepted
// precedent from progress_update.
// ---------------------------------------------------------------------------
export const TIMEOUT_SPEC: NodeConfigSpec = {
	fields: [
		{
			kind: 'code',
			bind: 'durationMsExpr',
			label: 'Deadline (ms)',
			description:
				'Rhai expression returning the race deadline in milliseconds. The body must complete within this window or the timeout output fires and cancellable in-flight body work is drained (HumanTask, SubWorkflow, nested Delay).\n\nv1 limitation: body cancellation reaches direct body children (one level deep) of cancellable kinds. AutomatedStep body children keep running until completion; nested Timeout/Loop body children are not auto-drained.',
			lang: 'rhai',
			minHeight: '40px',
			maxHeight: '100px'
		}
	]
};

// ---------------------------------------------------------------------------
// phase_update
//
// Original: PhaseUpdateNodeSection.svelte.  Fields:
//   phaseName   string required — InsertRefButton when scope.length>0
//   status      optional enum ('running'|'completed'|'failed'|'skipped'),
//               display-default 'running' (derived, NOT stored until user picks)
//   message     string optional, clear-to-undefined, InsertRefButton
//
// Quirks preserved:
//   - status: displayDefault:'running' for render-only fallback (not written
//     on mount; data key stays undefined until user explicitly picks)
//   - message: clearToUndefined:true (empty string → undefined)
//   - Both phaseName + message get InsertRefButton via the text/textarea branches
//     in FieldRenderer (text branch and textarea branch both wire InsertRefButton
//     when scope.length > 0)
//
// The trailing italic advisory ("Effective only within a named process…") is
// folded into the message field.description tail.
// ---------------------------------------------------------------------------
export const PHASE_UPDATE_SPEC: NodeConfigSpec = {
	fields: [
		{
			kind: 'text',
			bind: 'phaseName',
			label: 'Phase name',
			placeholder: 'e.g. Validation',
			description: 'Required. The named phase to mark on the owning process.'
		},
		{
			kind: 'select',
			bind: 'status',
			label: 'Status',
			displayDefault: 'running',
			options: [
				{ value: 'running', label: 'Running' },
				{ value: 'completed', label: 'Completed' },
				{ value: 'failed', label: 'Failed' },
				{ value: 'skipped', label: 'Skipped' }
			]
		},
		{
			kind: 'textarea',
			bind: 'message',
			label: 'Message (optional)',
			rows: 2,
			placeholder: 'e.g. Validating invoice {{ invoice_id }}',
			clearToUndefined: true,
			description:
				'{{ ref }} placeholders interpolate fields from this node\'s input scope — use the picker above for the in-scope set.\n\nEffective only within a named process (a Start with a Process Name upstream). Outside one this node passes the token through with no effect.'
		}
	]
};

// ---------------------------------------------------------------------------
// map
//
// Original: MapNodeSection.svelte.  Fields:
//   itemsRef    optional string ref — allowArrayBoundary:true (Map-specific)
//   itemVar     optional string — display-default 'item', font-mono
//   resultVar   optional string — font-mono
//   output      Port — the new 'port' authoring slot (PortsSection)
//
// Quirks preserved:
//   - itemsRef: allowArrayBoundary:true so array-typed fields are selectable.
//     FieldRenderer ref branch writes e.qualified and renders the selected-ref
//     echo line (the {#if value} <p font-mono> block added to FieldRenderer).
//   - itemVar: valueDefault:'item' — shown as live input value (not placeholder)
//     when data.itemVar is unset; mono:true for font-mono class.
//   - resultVar: mono:true. value fallback is '' (no valueDefault).
//   - output: port slot with synthesized default { id:'out', label:'Element',
//     fields:[] } and the exact emptyHint string from MapNodeSection.
//
// The three italic helper <p> blocks from the bespoke section fold into each
// field.description; the itemVar derived-echo ({itemVar}.<field>) is folded
// similarly (minor layout delta accepted — same as progress_update precedent).
// ---------------------------------------------------------------------------
export const MAP_SPEC: NodeConfigSpec = {
	fields: [
		{
			kind: 'ref',
			bind: 'itemsRef',
			label: 'Items to map over',
			placeholder: 'Pick an array field…',
			allowArrayBoundary: true,
			description:
				'The body runs once per element, in array order. A non-array value fails the run.'
		},
		{
			kind: 'text',
			bind: 'itemVar',
			label: 'Element variable',
			placeholder: 'item',
			valueDefault: 'item',
			mono: true,
			description:
				'Each body iteration reads the current element as <itemVar>.<field>. Defaults to "item".'
		},
		{
			kind: 'text',
			bind: 'resultVar',
			label: 'Collect field',
			placeholder: 'e.g. score',
			mono: true,
			description: 'One value per element, gathered in order into the collection.'
		},
		{
			kind: 'port',
			bind: 'output',
			label: 'Element shape',
			title: 'Element shape',
			emptyHint:
				'No element fields declared. The gathered collection borrows as an untyped array — declare fields to expose typed <map>[*].<field> refs downstream.',
			default: { id: 'out', label: 'Element', fields: [] }
		}
	]
};

// ---------------------------------------------------------------------------
// failure
//
// Original: FailureNodeSection.svelte — two fields:
//   failureMessage  string optional, clear-to-undefined, InsertRefButton,
//                   {{ ref }} template interpolation.
//   errorResultMapping  FieldMapping[] optional — the new mapping slot.
//
// Quirks preserved:
//   - failureMessage: clearToUndefined:true (empty string → undefined, never
//     persisted as '').  InsertRefButton is wired by the textarea branch in
//     FieldRenderer when scope.length > 0.
//   - errorResultMapping: mapping slot; absent === empty (defaults via ?? []);
//     new-row expression pre-seeded to literal "input" (NOT "" and NOT the
//     placeholder "input.code"); target = free-text Input; source = Textarea
//     + RefPicker INSERT helper (appendSnippet of e.qualified) gated on
//     scope.length > 0; per-row Trash + header Add hidden when readonly;
//     dashed empty-state; index-keyed; live commit; no reordering; no
//     per-row type/kind selector.
//
// The trailing italic advisory ("Marks the process failed but the workflow
// continues…") is folded into the failureMessage field.description tail to
// match the precedent from progress_update / phase_update.
// ---------------------------------------------------------------------------
export const FAILURE_SPEC: NodeConfigSpec = {
	fields: [
		{
			kind: 'textarea',
			bind: 'failureMessage',
			label: 'Failure message (optional)',
			rows: 2,
			placeholder: 'e.g. Validation failed for invoice {{ invoice_id }}',
			clearToUndefined: true,
			testid: 'input-failure-message',
			description:
				"{{ ref }} placeholders interpolate fields from this node's input scope — use the picker above for the in-scope set.\n\nMarks the process failed but the workflow continues to its End. Effective only within a named process (a Start with a Process Name upstream); outside one this node passes the token through with no effect."
		},
		{
			kind: 'mapping',
			bind: 'errorResultMapping',
			label: 'Error result',
			addTestid: 'btn-add-error-result-mapping',
			target: {
				placeholder: 'error_field',
				testid: 'input-error-result-target'
			},
			source: {
				widget: 'textarea',
				rows: 2,
				placeholder: 'input.code',
				testid: 'input-error-result-expr',
				allowArrayBoundary: false
			},
			newRow: { targetField: '', expression: 'input' },
			emptyHint:
				'The error envelope still carries the failure message as error.reason; adding fields attaches a structured error.value.'
		}
	]
};

// ---------------------------------------------------------------------------
// end
//
// Original: EndNodeSection.svelte — single FieldMapping[] list editor with a
// RefPicker as the primary widget, auto-fill-targetField on pick when blank,
// and a trailing explanatory <p> footer.
//
// Mapped to:
//   (1) MappingField { bind:'resultMapping', source.widget:'refpicker',
//       autoFillTargetWhenBlank:true } — reproduces the list editor 1:1,
//       including testids (btn-add-result-mapping, input-result-target),
//       the rename-preserve auto-fill policy, and per-row RefPicker/Input/Trash.
//   (2) footer on the MappingField — the trailing explanatory prose rendered
//       after the list (plain text; the <code>{ ok: true, value }</code> in
//       the original empty-state hint is preserved as plain text, minor
//       cosmetic regression accepted per design).
//
// The label 'Result mapping' is rendered by the mapping branch header; no
// custom slot is needed.  Parity confidence: HIGH.
// ---------------------------------------------------------------------------
export const END_SPEC: NodeConfigSpec = {
	fields: [
		{
			kind: 'mapping',
			bind: 'resultMapping',
			label: 'Result mapping',
			addTestid: 'btn-add-result-mapping',
			target: {
				placeholder: 'result_field',
				testid: 'input-result-target'
			},
			source: {
				widget: 'refpicker',
				placeholder: 'Pick source field…',
				autoFillTargetWhenBlank: true
			},
			newRow: { targetField: '', expression: '' },
			emptyHint:
				'No result. The workflow completes with no structured return — fully backward-compatible. Add one or more fields to build the success envelope ({ ok: true, value }) returned to callers.',
			footer:
				'Each row borrows a field from upstream (the compiler synthesizes a non-consuming read-arc) and assembles the success envelope. Rename the left field to publish under a different key. A Failure node upstream takes precedence — its error envelope is preserved instead of overwritten.'
		}
	]
};

// ---------------------------------------------------------------------------
// start
//
// Original: StartNodeSection.svelte — three regions:
//   (1) Process name: text Input with empty→null coercion + InsertRefButton.
//   (2) Initial token fields: PortsSection for the `initial` Port.
//   (3) Entrypoints: bespoke graph-relational trigger list + 'Add trigger'.
//
// Mapped to:
//   (1) TextField { bind:'processName', clearToNull:true } — the text branch
//       already wires InsertRefButton when scope.length>0; clearToNull:true
//       collapses '' → null (not undefined/''), matching the bespoke coercion.
//   (2) PortField { bind:'initial' } — drop-in onto the existing port slot;
//       identical PortsSection + verbatim write-back; synthesized sentinel
//       { id:'in', label:'Input', fields:[] } matches the bespoke default.
//       MUST preserve id:'in' so trigger edges' targetHandle pins correctly.
//   (3) CustomField { kind:'custom', component:'start.entrypoints' } — the
//       bespoke graph-relational Entrypoints region registered in custom-registry.ts.
//       Receives full section context (binding, nodeId, onselectnode) verbatim.
//
// Parity notes:
//   - processName clearToNull: empty → null (not undefined/''). Bespoke code:
//       onchange({ ...data, processName: value.length ? value : null })
//     FieldRenderer text branch with clearToNull:true emits null for '' exactly.
//   - initial sentinel: id:'in' (used by trigger edge targetHandle); matches
//     what StartNodeSection synthesizes: data.initial ?? { id:'in', label:'Input', fields:[] }
//   - Entrypoints testids: start-entrypoints, btn-add-trigger, trigger-link —
//     all owned by StartEntrypoints.svelte (verbatim lift, no testid change).
// ---------------------------------------------------------------------------
export const START_SPEC: NodeConfigSpec = {
	fields: [
		{
			kind: 'text',
			bind: 'processName',
			label: 'Process name',
			placeholder: "e.g. Invoice {{ invoice_id }}",
			clearToNull: true,
			description:
				'Optional. When set, each instance registers a named process (shown in the process list and completed when the workflow ends). {{ ref }} placeholders interpolate initial-token fields declared below. Leave empty to opt out.'
		},
		{
			kind: 'port',
			bind: 'initial',
			label: 'Initial token fields',
			title: 'Initial token fields',
			emptyHint:
				'No initial fields. Instances of this template will start with an empty token (system fields only). Add fields to require typed input at instance creation.',
			default: { id: 'in', label: 'Input', fields: [] }
		},
		{
			kind: 'custom',
			component: 'start.entrypoints',
			testid: 'start-entrypoints-wrapper'
		}
	]
};
