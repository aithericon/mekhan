/**
 * SPIKE — config-spec/specs.ts
 *
 * One NodeConfigSpec per migrated Tier-1 node.  Fields faithfully reproduce
 * what the bespoke section components offered (same data keys, same labels,
 * same widget semantics).
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
