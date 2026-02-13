import { describe, it, expect } from 'vitest';
import { compileToAIR, type CompileOutput } from './compile';
import type { WorkflowGraph } from '$lib/types/editor';

// --- Validation helper ---

type AIRValidationError = {
	field: string;
	message: string;
};

function validateAIR(air: CompileOutput['air']): AIRValidationError[] {
	const errors: AIRValidationError[] = [];

	// Top-level required fields
	if (!air.name) {
		errors.push({ field: 'name', message: 'AIR document must have a name' });
	}
	if (!Array.isArray(air.places)) {
		errors.push({ field: 'places', message: 'AIR document must have a places array' });
		return errors;
	}
	if (!Array.isArray(air.transitions)) {
		errors.push({ field: 'transitions', message: 'AIR document must have a transitions array' });
		return errors;
	}

	// Build place/transition lookup
	const placeIds = new Set(air.places.map((p) => p.id));
	const transitionIds = new Set(air.transitions.map((t) => t.id));

	// Validate places
	for (const place of air.places) {
		if (!place.id) errors.push({ field: `place`, message: 'Place missing id' });
		if (!place.name) errors.push({ field: `place[${place.id}]`, message: 'Place missing name' });
		if (!['state', 'resource', 'signal', 'terminal'].includes(place.type)) {
			errors.push({
				field: `place[${place.id}].type`,
				message: `Invalid place type: ${place.type}`
			});
		}
	}

	// Validate transitions
	for (const t of air.transitions) {
		if (!t.id) errors.push({ field: 'transition', message: 'Transition missing id' });
		if (!t.name) errors.push({ field: `transition[${t.id}]`, message: 'Transition missing name' });
		if (!Array.isArray(t.input_ports)) {
			errors.push({
				field: `transition[${t.id}].input_ports`,
				message: 'Transition missing input_ports'
			});
		}
		if (!Array.isArray(t.output_ports)) {
			errors.push({
				field: `transition[${t.id}].output_ports`,
				message: 'Transition missing output_ports'
			});
		}
		if (!Array.isArray(t.inputs)) {
			errors.push({ field: `transition[${t.id}].inputs`, message: 'Transition missing inputs' });
		}
		if (!Array.isArray(t.outputs)) {
			errors.push({
				field: `transition[${t.id}].outputs`,
				message: 'Transition missing outputs'
			});
		}
		if (!t.logic) {
			errors.push({ field: `transition[${t.id}].logic`, message: 'Transition missing logic' });
		}

		// Check arcs reference existing places
		for (const arc of t.inputs ?? []) {
			if (!placeIds.has(arc.place)) {
				errors.push({
					field: `transition[${t.id}].inputs`,
					message: `Input arc references non-existent place: ${arc.place}`
				});
			}
			// Check port exists in input_ports
			const portNames = (t.input_ports ?? []).map((p) => p.name);
			if (!portNames.includes(arc.port)) {
				errors.push({
					field: `transition[${t.id}].inputs`,
					message: `Input arc references non-existent port: ${arc.port}`
				});
			}
		}
		for (const arc of t.outputs ?? []) {
			if (!placeIds.has(arc.place)) {
				errors.push({
					field: `transition[${t.id}].outputs`,
					message: `Output arc references non-existent place: ${arc.place}`
				});
			}
			const portNames = (t.output_ports ?? []).map((p) => p.name);
			if (!portNames.includes(arc.port)) {
				errors.push({
					field: `transition[${t.id}].outputs`,
					message: `Output arc references non-existent port: ${arc.port}`
				});
			}
		}
	}

	// Check at least one place has initial_tokens (start)
	const hasInitial = air.places.some(
		(p) => p.initial_tokens && p.initial_tokens.length > 0
	);
	if (!hasInitial) {
		errors.push({
			field: 'places',
			message: 'AIR must have at least one place with initial_tokens (start)'
		});
	}

	// Check at least one terminal place (end)
	const hasTerminal = air.places.some((p) => p.type === 'terminal');
	if (!hasTerminal) {
		errors.push({
			field: 'places',
			message: 'AIR must have at least one terminal place (end)'
		});
	}

	return errors;
}

// --- Test graphs ---

function makeSimpleGraph(): WorkflowGraph {
	return {
		nodes: [
			{ id: 'start-1', type: 'start', position: { x: 0, y: 0 }, data: { type: 'start', label: 'Start' } },
			{ id: 'end-1', type: 'end', position: { x: 0, y: 200 }, data: { type: 'end', label: 'End' } }
		],
		edges: [
			{ id: 'e1', source: 'start-1', target: 'end-1', type: 'sequence' }
		]
	};
}

function makeHumanTaskGraph(): WorkflowGraph {
	return {
		nodes: [
			{ id: 'start-1', type: 'start', position: { x: 0, y: 0 }, data: { type: 'start', label: 'Start' } },
			{
				id: 'ht-1', type: 'human_task', position: { x: 0, y: 100 },
				data: {
					type: 'human_task', label: 'Review Task', taskTitle: 'Review',
					steps: [{
						id: 'step1', title: 'Step 1',
						blocks: [{ type: 'input', field: { name: 'approved', label: 'Approved?', kind: 'checkbox' } }]
					}]
				}
			},
			{ id: 'end-1', type: 'end', position: { x: 0, y: 200 }, data: { type: 'end', label: 'End' } }
		],
		edges: [
			{ id: 'e1', source: 'start-1', target: 'ht-1', type: 'sequence' },
			{ id: 'e2', source: 'ht-1', target: 'end-1', type: 'sequence' }
		]
	};
}

function makeAutomatedStepGraph(): WorkflowGraph {
	return {
		nodes: [
			{ id: 'start-1', type: 'start', position: { x: 0, y: 0 }, data: { type: 'start', label: 'Start' } },
			{
				id: 'auto-1', type: 'automated_step', position: { x: 0, y: 100 },
				data: {
					type: 'automated_step', label: 'Run Script',
					executionSpec: { backendType: 'python', config: { script: 'print("hello")' } }
				}
			},
			{ id: 'end-1', type: 'end', position: { x: 0, y: 200 }, data: { type: 'end', label: 'End' } }
		],
		edges: [
			{ id: 'e1', source: 'start-1', target: 'auto-1', type: 'sequence' },
			{ id: 'e2', source: 'auto-1', target: 'end-1', type: 'sequence' }
		]
	};
}

function makeDecisionGraph(): WorkflowGraph {
	return {
		nodes: [
			{ id: 'start-1', type: 'start', position: { x: 0, y: 0 }, data: { type: 'start', label: 'Start' } },
			{
				id: 'dec-1', type: 'decision', position: { x: 0, y: 100 },
				data: {
					type: 'decision', label: 'Check Value',
					conditions: [
						{ edgeId: 'branch-a', label: 'Branch A', guard: 'input.value > 10' },
						{ edgeId: 'branch-b', label: 'Branch B', guard: 'input.value <= 10' }
					]
				}
			},
			{ id: 'end-a', type: 'end', position: { x: -100, y: 200 }, data: { type: 'end', label: 'End A' } },
			{ id: 'end-b', type: 'end', position: { x: 100, y: 200 }, data: { type: 'end', label: 'End B' } }
		],
		edges: [
			{ id: 'e1', source: 'start-1', target: 'dec-1', type: 'sequence' },
			{ id: 'e-a', source: 'dec-1', target: 'end-a', type: 'conditional', sourceHandle: 'branch-a' },
			{ id: 'e-b', source: 'dec-1', target: 'end-b', type: 'conditional', sourceHandle: 'branch-b' }
		]
	};
}

function makeParallelGraph(): WorkflowGraph {
	return {
		nodes: [
			{ id: 'start-1', type: 'start', position: { x: 0, y: 0 }, data: { type: 'start', label: 'Start' } },
			{ id: 'split-1', type: 'parallel_split', position: { x: 0, y: 100 }, data: { type: 'parallel_split', label: 'Split' } },
			{
				id: 'ht-a', type: 'human_task', position: { x: -100, y: 200 },
				data: {
					type: 'human_task', label: 'Task A', taskTitle: 'Task A',
					steps: [{ id: 's1', title: 'Step', blocks: [{ type: 'input', field: { name: 'a_result', label: 'Result A', kind: 'text' } }] }]
				}
			},
			{
				id: 'ht-b', type: 'human_task', position: { x: 100, y: 200 },
				data: {
					type: 'human_task', label: 'Task B', taskTitle: 'Task B',
					steps: [{ id: 's2', title: 'Step', blocks: [{ type: 'input', field: { name: 'b_result', label: 'Result B', kind: 'text' } }] }]
				}
			},
			{ id: 'join-1', type: 'parallel_join', position: { x: 0, y: 300 }, data: { type: 'parallel_join', label: 'Join' } },
			{ id: 'end-1', type: 'end', position: { x: 0, y: 400 }, data: { type: 'end', label: 'End' } }
		],
		edges: [
			{ id: 'e1', source: 'start-1', target: 'split-1', type: 'sequence' },
			{ id: 'e2', source: 'split-1', target: 'ht-a', type: 'sequence' },
			{ id: 'e3', source: 'split-1', target: 'ht-b', type: 'sequence' },
			{ id: 'e4', source: 'ht-a', target: 'join-1', type: 'sequence' },
			{ id: 'e5', source: 'ht-b', target: 'join-1', type: 'sequence' },
			{ id: 'e6', source: 'join-1', target: 'end-1', type: 'sequence' }
		]
	};
}

function makeLoopGraph(): WorkflowGraph {
	return {
		nodes: [
			{ id: 'start-1', type: 'start', position: { x: 0, y: 0 }, data: { type: 'start', label: 'Start' } },
			{
				id: 'loop-1', type: 'loop', position: { x: 0, y: 100 },
				data: { type: 'loop', label: 'Retry Loop', maxIterations: 3, loopCondition: 'input.retry == true' }
			},
			{ id: 'end-1', type: 'end', position: { x: 0, y: 200 }, data: { type: 'end', label: 'End' } }
		],
		edges: [
			{ id: 'e1', source: 'start-1', target: 'loop-1', type: 'sequence' },
			{ id: 'e2', source: 'loop-1', target: 'end-1', type: 'sequence' }
		]
	};
}

// --- Tests ---

describe('validateAIR', () => {
	it('validates a simple Start -> End AIR as valid', () => {
		const result = compileToAIR(makeSimpleGraph(), 'Simple');
		expect(result.errors).toHaveLength(0);
		const validationErrors = validateAIR(result.air);
		expect(validationErrors).toHaveLength(0);
	});

	it('validates Start -> HumanTask -> End AIR as valid', () => {
		const result = compileToAIR(makeHumanTaskGraph(), 'HumanTask Flow');
		expect(result.errors).toHaveLength(0);
		const validationErrors = validateAIR(result.air);
		expect(validationErrors).toHaveLength(0);
	});

	it('validates Start -> AutomatedStep -> End AIR as valid', () => {
		const result = compileToAIR(makeAutomatedStepGraph(), 'AutoStep Flow');
		expect(result.errors).toHaveLength(0);
		const validationErrors = validateAIR(result.air);
		expect(validationErrors).toHaveLength(0);
	});

	it('validates Start -> Decision(A,B) -> End AIR as valid', () => {
		const result = compileToAIR(makeDecisionGraph(), 'Decision Flow');
		expect(result.errors).toHaveLength(0);
		const validationErrors = validateAIR(result.air);
		expect(validationErrors).toHaveLength(0);
	});

	it('validates Start -> ParallelSplit -> (A,B) -> ParallelJoin -> End AIR as valid', () => {
		const result = compileToAIR(makeParallelGraph(), 'Parallel Flow');
		expect(result.errors).toHaveLength(0);
		const validationErrors = validateAIR(result.air);
		expect(validationErrors).toHaveLength(0);
	});

	it('validates Start -> Loop -> End AIR as valid', () => {
		const result = compileToAIR(makeLoopGraph(), 'Loop Flow');
		expect(result.errors).toHaveLength(0);
		const validationErrors = validateAIR(result.air);
		expect(validationErrors).toHaveLength(0);
	});

	it('detects missing initial_tokens', () => {
		const air = {
			name: 'Bad',
			places: [{ id: 'p1', name: 'P1', type: 'state' as const }],
			transitions: [],
			groups: [],
			definitions: {}
		};
		const errs = validateAIR(air);
		expect(errs.some((e) => e.message.includes('initial_tokens'))).toBe(true);
	});

	it('detects missing terminal place', () => {
		const air = {
			name: 'Bad',
			places: [{ id: 'p1', name: 'P1', type: 'state' as const, initial_tokens: [{}] }],
			transitions: [],
			groups: [],
			definitions: {}
		};
		const errs = validateAIR(air);
		expect(errs.some((e) => e.message.includes('terminal'))).toBe(true);
	});

	it('detects arc referencing non-existent place', () => {
		const air = {
			name: 'Bad',
			places: [
				{ id: 'p1', name: 'Start', type: 'state' as const, initial_tokens: [{}] },
				{ id: 'p2', name: 'End', type: 'terminal' as const }
			],
			transitions: [{
				id: 't1', name: 'T1',
				input_ports: [{ name: 'input', cardinality: 'single' as const }],
				output_ports: [{ name: 'output', cardinality: 'single' as const }],
				inputs: [{ place: 'p1', port: 'input' }],
				outputs: [{ place: 'p_nonexistent', port: 'output' }],
				logic: { type: 'rhai' as const, source: '#{ output: input }' }
			}],
			groups: [],
			definitions: {}
		};
		const errs = validateAIR(air);
		expect(errs.some((e) => e.message.includes('non-existent place'))).toBe(true);
	});

	it('detects arc referencing non-existent port', () => {
		const air = {
			name: 'Bad',
			places: [
				{ id: 'p1', name: 'Start', type: 'state' as const, initial_tokens: [{}] },
				{ id: 'p2', name: 'End', type: 'terminal' as const }
			],
			transitions: [{
				id: 't1', name: 'T1',
				input_ports: [{ name: 'input', cardinality: 'single' as const }],
				output_ports: [{ name: 'output', cardinality: 'single' as const }],
				inputs: [{ place: 'p1', port: 'wrong_port' }],
				outputs: [{ place: 'p2', port: 'output' }],
				logic: { type: 'rhai' as const, source: '#{ output: input }' }
			}],
			groups: [],
			definitions: {}
		};
		const errs = validateAIR(air);
		expect(errs.some((e) => e.message.includes('non-existent port'))).toBe(true);
	});
});

// --- Exported for use in other test files ---
export { validateAIR, makeSimpleGraph, makeHumanTaskGraph, makeAutomatedStepGraph, makeDecisionGraph, makeParallelGraph, makeLoopGraph };
export type { AIRValidationError };
