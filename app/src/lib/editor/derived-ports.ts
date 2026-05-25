// Phase 4: TS twin of `WorkflowNodeData::output_ports` for the variants
// whose ports are *derived* from inner config rather than carried as
// editable state. The editor displays these read-only — keeping them in
// sync with the backend definition ensures the visual port summary on each
// node card matches what the compiler will see at publish.

import type { components } from '$lib/api/schema';

type WorkflowNodeData = components['schemas']['WorkflowNodeData'];
type Port = components['schemas']['Port'];
type PortField = components['schemas']['PortField'];
type FieldKind = components['schemas']['FieldKind'];
type TaskFieldKind = components['schemas']['TaskFieldKind'];

export function outputPortsFor(data: WorkflowNodeData): Port[] {
	switch (data.type) {
		case 'start':
			return data.initial ? [data.initial] : [];
		case 'automated_step':
			// Success output + the always-present "error" output (retries
			// exhausted / infra failure). Mirrors the compiler's NodePorts.
			return [
				...(data.output ? [data.output] : []),
				{ id: 'error', label: 'On error', fields: [] }
			];
		case 'human_task':
			return [deriveHumanTaskOutputPort(data)];
		case 'decision':
			return deriveDecisionOutputPorts(data);
		case 'parallel_split':
		case 'parallel_join':
		case 'loop':
		case 'scope':
		case 'phase_update':
		case 'progress_update':
		case 'failure':
		case 'trigger':
			// Triggers "wear the shape" of the target port — the editor resolves
			// it at render time via the outgoing edge. Statically we expose an
			// empty pass-through port so handles render correctly.
			return [{ id: 'out', label: 'Output', fields: [] }];
		case 'join':
			// Join carries an explicit output Port whose fields describe what
			// downstream `<slug>.<field>` borrows can read.
			return [data.output ?? { id: 'out', label: 'Output', fields: [] }];
		default:
			return [];
	}
}

export function inputPortsFor(data: WorkflowNodeData): Port[] {
	switch (data.type) {
		case 'start':
			return [];
		case 'end':
			return data.terminal ? [data.terminal] : [];
		case 'automated_step':
			return data.input ? [data.input] : [];
		case 'human_task':
		case 'decision':
		case 'parallel_split':
		case 'parallel_join':
		case 'join':
		case 'loop':
		case 'scope':
		case 'phase_update':
		case 'progress_update':
		case 'failure':
			return [{ id: 'in', label: 'Input', fields: [] }];
		case 'trigger':
			// Triggers are never edge targets — no input port.
			return [];
		default:
			return [];
	}
}

type HumanTaskNodeData = Extract<WorkflowNodeData, { type: 'human_task' }>;
type DecisionNodeData = Extract<WorkflowNodeData, { type: 'decision' }>;

function deriveHumanTaskOutputPort(data: HumanTaskNodeData): Port {
	const seen = new Set<string>();
	const fields: PortField[] = [];
	for (const step of data.steps ?? []) {
		for (const block of step.blocks ?? []) {
			if (block.type !== 'input') continue;
			const f = block.field;
			if (seen.has(f.name)) continue;
			seen.add(f.name);
			fields.push({
				name: f.name,
				label: f.label,
				kind: taskFieldKindToFieldKind(f.kind),
				required: f.required ?? false,
				options: f.options ?? undefined
			});
		}
	}
	return { id: 'out', label: 'Output', fields };
}

function deriveDecisionOutputPorts(data: DecisionNodeData): Port[] {
	const out: Port[] = (data.conditions ?? []).map((c) => ({
		id: c.edgeId,
		label: c.label,
		fields: []
	}));
	if (data.defaultBranch) {
		out.push({ id: data.defaultBranch, label: 'Default', fields: [] });
	}
	return out;
}

function taskFieldKindToFieldKind(k: TaskFieldKind): FieldKind {
	switch (k) {
		case 'text':
			return 'text';
		case 'textarea':
			return 'textarea';
		case 'number':
			return 'number';
		case 'select':
			return 'select';
		case 'checkbox':
			return 'bool';
		case 'file':
			return 'file';
		case 'signature':
			return 'signature';
		default:
			return 'text';
	}
}

/**
 * Whether the editor offers a UI to edit this node kind's ports. Used by the
 * NodePropertyPanel to switch between an editable PortsSection (Start,
 * AutomatedStep, End.terminal, Scope) and a read-only "Derived" summary.
 */
export function hasEditableOutputPorts(kind: WorkflowNodeData['type']): boolean {
	return kind === 'start' || kind === 'automated_step';
}

export function hasEditableInputPorts(kind: WorkflowNodeData['type']): boolean {
	return kind === 'end' || kind === 'automated_step';
}
