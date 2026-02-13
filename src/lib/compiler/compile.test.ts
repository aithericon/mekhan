import { describe, it, expect } from 'vitest';
import { compileToAIR } from './compile';
import { validateGraph } from './validate';
import type { WorkflowGraph } from '$lib/types/editor';
import { writeFileSync, mkdirSync } from 'fs';
import { join } from 'path';

// ── Fixtures ──────────────────────────────────────────────────────────

/** Minimal Start -> End graph */
function startEndGraph(): WorkflowGraph {
	return {
		nodes: [
			{ id: 'n-start', type: 'start', position: { x: 0, y: 0 }, data: { type: 'start', label: 'Start' } },
			{ id: 'n-end', type: 'end', position: { x: 300, y: 0 }, data: { type: 'end', label: 'End' } }
		],
		edges: [
			{ id: 'e-1', source: 'n-start', target: 'n-end', type: 'sequence' }
		]
	};
}

/** Start -> HumanTask -> End graph */
function startHumanTaskEndGraph(): WorkflowGraph {
	return {
		nodes: [
			{ id: 'n-start', type: 'start', position: { x: 0, y: 0 }, data: { type: 'start', label: 'Start' } },
			{
				id: 'n-ht',
				type: 'human_task',
				position: { x: 200, y: 0 },
				data: {
					type: 'human_task',
					label: 'Review',
					taskTitle: 'Review Document',
					steps: [
						{
							id: 'step-1',
							title: 'Step 1',
							blocks: [
								{
									type: 'input' as const,
									field: { name: 'approval', label: 'Approved?', kind: 'checkbox' as const, required: true }
								}
							]
						}
					]
				}
			},
			{ id: 'n-end', type: 'end', position: { x: 500, y: 0 }, data: { type: 'end', label: 'End' } }
		],
		edges: [
			{ id: 'e-1', source: 'n-start', target: 'n-ht', type: 'sequence' },
			{ id: 'e-2', source: 'n-ht', target: 'n-end', type: 'sequence' }
		]
	};
}

// ── Validation Tests ──────────────────────────────────────────────────

describe('validateGraph', () => {
	it('returns no errors for a valid Start -> End graph', () => {
		const errors = validateGraph(startEndGraph());
		expect(errors).toHaveLength(0);
	});

	it('returns no errors for a valid Start -> HumanTask -> End graph', () => {
		const errors = validateGraph(startHumanTaskEndGraph());
		expect(errors).toHaveLength(0);
	});

	it('catches missing Start node', () => {
		const graph: WorkflowGraph = {
			nodes: [
				{ id: 'n-end', type: 'end', position: { x: 0, y: 0 }, data: { type: 'end', label: 'End' } }
			],
			edges: []
		};
		const errors = validateGraph(graph);
		expect(errors.some((e) => e.message.includes('Start node'))).toBe(true);
	});

	it('catches missing End node', () => {
		const graph: WorkflowGraph = {
			nodes: [
				{ id: 'n-start', type: 'start', position: { x: 0, y: 0 }, data: { type: 'start', label: 'Start' } }
			],
			edges: []
		};
		const errors = validateGraph(graph);
		expect(errors.some((e) => e.message.includes('End node'))).toBe(true);
	});

	it('catches disconnected nodes', () => {
		const graph: WorkflowGraph = {
			nodes: [
				{ id: 'n-start', type: 'start', position: { x: 0, y: 0 }, data: { type: 'start', label: 'Start' } },
				{ id: 'n-end', type: 'end', position: { x: 300, y: 0 }, data: { type: 'end', label: 'End' } },
				{
					id: 'n-island',
					type: 'human_task',
					position: { x: 200, y: 200 },
					data: {
						type: 'human_task',
						label: 'Island Task',
						taskTitle: 'Orphan',
						steps: [{ id: 's1', title: 'S1', blocks: [] }]
					}
				}
			],
			edges: [
				{ id: 'e-1', source: 'n-start', target: 'n-end', type: 'sequence' }
			]
		};
		const errors = validateGraph(graph);
		expect(errors.some((e) => e.message.includes('not reachable'))).toBe(true);
	});

	it('catches empty graph', () => {
		const errors = validateGraph({ nodes: [], edges: [] });
		expect(errors.some((e) => e.message.includes('no nodes'))).toBe(true);
	});

	it('catches Start with no outgoing connections', () => {
		const graph: WorkflowGraph = {
			nodes: [
				{ id: 'n-start', type: 'start', position: { x: 0, y: 0 }, data: { type: 'start', label: 'Start' } },
				{ id: 'n-end', type: 'end', position: { x: 300, y: 0 }, data: { type: 'end', label: 'End' } }
			],
			edges: []
		};
		const errors = validateGraph(graph);
		expect(errors.some((e) => e.message.includes('no outgoing connections'))).toBe(true);
	});

	it('catches End with no incoming connections', () => {
		const graph: WorkflowGraph = {
			nodes: [
				{ id: 'n-start', type: 'start', position: { x: 0, y: 0 }, data: { type: 'start', label: 'Start' } },
				{ id: 'n-end', type: 'end', position: { x: 300, y: 0 }, data: { type: 'end', label: 'End' } }
			],
			edges: []
		};
		const errors = validateGraph(graph);
		expect(errors.some((e) => e.message.includes('no incoming connections'))).toBe(true);
	});

	it('catches human task with no steps', () => {
		const graph: WorkflowGraph = {
			nodes: [
				{ id: 'n-start', type: 'start', position: { x: 0, y: 0 }, data: { type: 'start', label: 'Start' } },
				{
					id: 'n-ht',
					type: 'human_task',
					position: { x: 200, y: 0 },
					data: { type: 'human_task', label: 'Empty Task', taskTitle: 'No Steps', steps: [] }
				},
				{ id: 'n-end', type: 'end', position: { x: 400, y: 0 }, data: { type: 'end', label: 'End' } }
			],
			edges: [
				{ id: 'e-1', source: 'n-start', target: 'n-ht', type: 'sequence' },
				{ id: 'e-2', source: 'n-ht', target: 'n-end', type: 'sequence' }
			]
		};
		const errors = validateGraph(graph);
		expect(errors.some((e) => e.message.includes('no steps'))).toBe(true);
	});

	it('catches multiple Start nodes', () => {
		const graph: WorkflowGraph = {
			nodes: [
				{ id: 'n-start-1', type: 'start', position: { x: 0, y: 0 }, data: { type: 'start', label: 'Start 1' } },
				{ id: 'n-start-2', type: 'start', position: { x: 0, y: 100 }, data: { type: 'start', label: 'Start 2' } },
				{ id: 'n-end', type: 'end', position: { x: 300, y: 0 }, data: { type: 'end', label: 'End' } }
			],
			edges: [
				{ id: 'e-1', source: 'n-start-1', target: 'n-end', type: 'sequence' },
				{ id: 'e-2', source: 'n-start-2', target: 'n-end', type: 'sequence' }
			]
		};
		const errors = validateGraph(graph);
		expect(errors.some((e) => e.message.includes('Only one Start'))).toBe(true);
	});
});

// ── Compilation Tests ─────────────────────────────────────────────────

describe('compileToAIR', () => {
	it('compiles Start -> End to valid AIR', () => {
		const result = compileToAIR(startEndGraph(), 'Simple Flow');
		expect(result.errors).toHaveLength(0);
		expect(result.air.name).toBe('Simple Flow');

		// Should have places for start and end
		expect(result.air.places.length).toBeGreaterThanOrEqual(2);

		// Start place should have initial tokens
		const startPlace = result.air.places.find((p) => p.id === 'p_n-start_ready');
		expect(startPlace).toBeDefined();
		expect(startPlace!.type).toBe('state');
		expect(startPlace!.initial_tokens).toBeDefined();
		expect(startPlace!.initial_tokens!.length).toBeGreaterThan(0);

		// End place should be terminal
		const endPlace = result.air.places.find((p) => p.id === 'p_n-end_done');
		expect(endPlace).toBeDefined();
		expect(endPlace!.type).toBe('terminal');

		// Should have at least one transition wiring them
		expect(result.air.transitions.length).toBeGreaterThan(0);
	});

	it('compiles Start -> HumanTask -> End with correct places and transitions', () => {
		const result = compileToAIR(startHumanTaskEndGraph(), 'HT Flow');
		expect(result.errors).toHaveLength(0);

		const { air } = result;

		// Check top-level places
		expect(air.places.find((p) => p.id === 'p_n-start_ready')).toBeDefined();
		expect(air.places.find((p) => p.id === 'p_n-end_done')).toBeDefined();

		// HumanTask should create input, active, signal, output places
		expect(air.places.find((p) => p.id === 'p_n-ht_input')).toBeDefined();
		expect(air.places.find((p) => p.id === 'p_n-ht_active')).toBeDefined();
		expect(air.places.find((p) => p.id === 'p_n-ht_signal')).toBeDefined();
		expect(air.places.find((p) => p.id === 'p_n-ht_output')).toBeDefined();

		// HumanTask should create request and finalize transitions
		const requestT = air.transitions.find((t) => t.id === 't_n-ht_request');
		expect(requestT).toBeDefined();
		expect(requestT!.logic.type).toBe('effect');

		const finalizeT = air.transitions.find((t) => t.id === 't_n-ht_finalize');
		expect(finalizeT).toBeDefined();

		// Should have a group
		expect(air.groups.find((g) => g.id === 'grp_n-ht')).toBeDefined();

		// Edge transitions should wire start -> ht_input and ht_output -> end
		expect(air.transitions.some((t) => t.id.startsWith('t_edge_'))).toBe(true);
	});

	it('returns errors for invalid graph', () => {
		const result = compileToAIR({ nodes: [], edges: [] }, 'Invalid');
		expect(result.errors.length).toBeGreaterThan(0);
		// AIR should still be present but empty
		expect(result.air.places).toHaveLength(0);
		expect(result.air.transitions).toHaveLength(0);
	});

	it('includes description when provided', () => {
		const result = compileToAIR(startEndGraph(), 'Named Flow', 'A test description');
		expect(result.errors).toHaveLength(0);
		expect(result.air.description).toBe('A test description');
	});

	it('compiles automated step correctly', () => {
		const graph: WorkflowGraph = {
			nodes: [
				{ id: 'n-start', type: 'start', position: { x: 0, y: 0 }, data: { type: 'start', label: 'Start' } },
				{
					id: 'n-auto',
					type: 'automated_step',
					position: { x: 200, y: 0 },
					data: {
						type: 'automated_step',
						label: 'Run Script',
						executionSpec: { backendType: 'python', config: {} }
					}
				},
				{ id: 'n-end', type: 'end', position: { x: 400, y: 0 }, data: { type: 'end', label: 'End' } }
			],
			edges: [
				{ id: 'e-1', source: 'n-start', target: 'n-auto', type: 'sequence' },
				{ id: 'e-2', source: 'n-auto', target: 'n-end', type: 'sequence' }
			]
		};
		const result = compileToAIR(graph, 'Auto Flow');
		expect(result.errors).toHaveLength(0);

		// AutomatedStep should produce submit transition with executor_submit effect
		const submitT = result.air.transitions.find((t) => t.id === 't_n-auto_submit');
		expect(submitT).toBeDefined();
		expect(submitT!.logic.type).toBe('effect');
		if (submitT!.logic.type === 'effect') {
			expect(submitT!.logic.handler_id).toBe('executor_submit');
			expect(submitT!.logic.config.backend_type).toBe('python');
		}
	});

	it('compiles loop node with guard expressions', () => {
		const graph: WorkflowGraph = {
			nodes: [
				{ id: 'n-start', type: 'start', position: { x: 0, y: 0 }, data: { type: 'start', label: 'Start' } },
				{
					id: 'n-loop',
					type: 'loop',
					position: { x: 200, y: 0 },
					data: { type: 'loop', label: 'Retry Loop', maxIterations: 5, loopCondition: 'true' }
				},
				{ id: 'n-end', type: 'end', position: { x: 400, y: 0 }, data: { type: 'end', label: 'End' } }
			],
			edges: [
				{ id: 'e-1', source: 'n-start', target: 'n-loop', type: 'sequence' },
				{ id: 'e-2', source: 'n-loop', target: 'n-end', type: 'sequence' }
			]
		};
		const result = compileToAIR(graph, 'Loop Flow');
		expect(result.errors).toHaveLength(0);

		// Loop should have enter, continue, and exit transitions
		expect(result.air.transitions.find((t) => t.id === 't_n-loop_enter')).toBeDefined();
		expect(result.air.transitions.find((t) => t.id === 't_n-loop_continue')).toBeDefined();
		expect(result.air.transitions.find((t) => t.id === 't_n-loop_exit')).toBeDefined();

		// Continue guard should reference max iterations
		const continueT = result.air.transitions.find((t) => t.id === 't_n-loop_continue')!;
		expect(continueT.guard).toBeDefined();
		expect(continueT.guard!.source).toContain('5');
	});
});

// ── Node Ordering Bug Regression ──────────────────────────────────────

describe('decision/parallel_split ordering independence', () => {
	it('compiles Decision -> HumanTask correctly even when Decision appears BEFORE target in nodes array', () => {
		// This is the key regression test: the Decision node appears before
		// its branch targets in the nodes array, so target nodes are NOT yet
		// expanded when expandDecision runs. The old code tried to wire
		// targets during expansion and silently dropped the wiring.
		const graph: WorkflowGraph = {
			nodes: [
				{ id: 'n-start', type: 'start', position: { x: 0, y: 0 }, data: { type: 'start', label: 'Start' } },
				{
					id: 'n-decision',
					type: 'decision',
					position: { x: 200, y: 0 },
					data: {
						type: 'decision',
						label: 'Check',
						conditions: [
							{ edgeId: 'cond-yes', label: 'Yes', guard: 'input.approved == true' }
						]
					}
				},
				// Target nodes appear AFTER the decision in the array
				{
					id: 'n-ht',
					type: 'human_task',
					position: { x: 400, y: -100 },
					data: {
						type: 'human_task',
						label: 'Approve',
						taskTitle: 'Approval',
						steps: [{ id: 's1', title: 'Step 1', blocks: [] }]
					}
				},
				{ id: 'n-end', type: 'end', position: { x: 400, y: 100 }, data: { type: 'end', label: 'End' } }
			],
			edges: [
				{ id: 'e-1', source: 'n-start', target: 'n-decision', type: 'sequence' },
				{ id: 'e-2', source: 'n-decision', target: 'n-ht', sourceHandle: 'cond-yes', type: 'conditional' },
				{ id: 'e-3', source: 'n-decision', target: 'n-end', type: 'conditional' },
				{ id: 'e-4', source: 'n-ht', target: 'n-end', type: 'sequence' }
			]
		};

		const result = compileToAIR(graph, 'Decision Ordering Test');
		expect(result.errors).toHaveLength(0);

		const { air } = result;

		// Collect all place ids referenced by transitions
		const allPlaceIds = new Set(air.places.map((p) => p.id));

		// Every transition's inputs and outputs must reference valid places
		for (const t of air.transitions) {
			for (const inp of t.inputs) {
				expect(allPlaceIds.has(inp.place)).toBe(true);
			}
			for (const out of t.outputs) {
				expect(allPlaceIds.has(out.place)).toBe(true);
			}
		}

		// There must be a pass-through edge transition wiring decision branch -> ht_input
		const edgeTransition = air.transitions.find((t) => t.id === 't_edge_e-2');
		expect(edgeTransition).toBeDefined();
		expect(edgeTransition!.inputs[0].place).toBe('p_n-decision_out_cond-yes');
		expect(edgeTransition!.outputs[0].place).toBe('p_n-ht_input');

		// And a pass-through for the default branch -> end
		const defaultEdgeTransition = air.transitions.find((t) => t.id === 't_edge_e-3');
		expect(defaultEdgeTransition).toBeDefined();
		expect(defaultEdgeTransition!.outputs[0].place).toBe('p_n-end_done');
	});

	it('compiles ParallelSplit -> targets correctly regardless of node order', () => {
		const graph: WorkflowGraph = {
			nodes: [
				{ id: 'n-start', type: 'start', position: { x: 0, y: 0 }, data: { type: 'start', label: 'Start' } },
				{
					id: 'n-split',
					type: 'parallel_split',
					position: { x: 200, y: 0 },
					data: { type: 'parallel_split', label: 'Fork' }
				},
				// Targets appear AFTER the split
				{
					id: 'n-ht-a',
					type: 'human_task',
					position: { x: 400, y: -100 },
					data: {
						type: 'human_task',
						label: 'Task A',
						taskTitle: 'A',
						steps: [{ id: 's1', title: 'Step', blocks: [] }]
					}
				},
				{
					id: 'n-ht-b',
					type: 'human_task',
					position: { x: 400, y: 100 },
					data: {
						type: 'human_task',
						label: 'Task B',
						taskTitle: 'B',
						steps: [{ id: 's2', title: 'Step', blocks: [] }]
					}
				},
				{
					id: 'n-join',
					type: 'parallel_join',
					position: { x: 600, y: 0 },
					data: { type: 'parallel_join', label: 'Join' }
				},
				{ id: 'n-end', type: 'end', position: { x: 800, y: 0 }, data: { type: 'end', label: 'End' } }
			],
			edges: [
				{ id: 'e-1', source: 'n-start', target: 'n-split', type: 'sequence' },
				{ id: 'e-2', source: 'n-split', target: 'n-ht-a', type: 'sequence' },
				{ id: 'e-3', source: 'n-split', target: 'n-ht-b', type: 'sequence' },
				{ id: 'e-4', source: 'n-ht-a', target: 'n-join', type: 'sequence' },
				{ id: 'e-5', source: 'n-ht-b', target: 'n-join', type: 'sequence' },
				{ id: 'e-6', source: 'n-join', target: 'n-end', type: 'sequence' }
			]
		};

		const result = compileToAIR(graph, 'ParallelSplit Ordering Test');
		expect(result.errors).toHaveLength(0);

		const { air } = result;

		// All transition arcs must reference valid places
		const allPlaceIds = new Set(air.places.map((p) => p.id));
		for (const t of air.transitions) {
			for (const inp of t.inputs) {
				expect(allPlaceIds.has(inp.place)).toBe(true);
			}
			for (const out of t.outputs) {
				expect(allPlaceIds.has(out.place)).toBe(true);
			}
		}

		// Edge transitions from split branches to targets must exist
		const edgeSplitA = air.transitions.find((t) => t.id === 't_edge_e-2');
		expect(edgeSplitA).toBeDefined();
		expect(edgeSplitA!.inputs[0].place).toBe('p_n-split_out_0');
		expect(edgeSplitA!.outputs[0].place).toBe('p_n-ht-a_input');

		const edgeSplitB = air.transitions.find((t) => t.id === 't_edge_e-3');
		expect(edgeSplitB).toBeDefined();
		expect(edgeSplitB!.inputs[0].place).toBe('p_n-split_out_1');
		expect(edgeSplitB!.outputs[0].place).toBe('p_n-ht-b_input');
	});

	it('all transitions reference valid places in a complex decision graph', () => {
		// A more thorough structural integrity check: compile a decision graph
		// and verify no dangling place references exist
		const graph: WorkflowGraph = {
			nodes: [
				{ id: 'n-start', type: 'start', position: { x: 0, y: 0 }, data: { type: 'start', label: 'Start' } },
				{
					id: 'n-dec',
					type: 'decision',
					position: { x: 200, y: 0 },
					data: {
						type: 'decision',
						label: 'Route',
						conditions: [
							{ edgeId: 'branch-a', label: 'Path A', guard: 'input.x > 0' },
							{ edgeId: 'branch-b', label: 'Path B', guard: 'input.x <= 0' }
						]
					}
				},
				{ id: 'n-end-a', type: 'end', position: { x: 400, y: -100 }, data: { type: 'end', label: 'End A' } },
				{ id: 'n-end-b', type: 'end', position: { x: 400, y: 100 }, data: { type: 'end', label: 'End B' } }
			],
			edges: [
				{ id: 'e-1', source: 'n-start', target: 'n-dec', type: 'sequence' },
				{ id: 'e-2', source: 'n-dec', target: 'n-end-a', sourceHandle: 'branch-a', type: 'conditional' },
				{ id: 'e-3', source: 'n-dec', target: 'n-end-b', sourceHandle: 'branch-b', type: 'conditional' }
			]
		};

		const result = compileToAIR(graph, 'Multi-Branch Decision');
		expect(result.errors).toHaveLength(0);

		const { air } = result;
		const allPlaceIds = new Set(air.places.map((p) => p.id));

		for (const t of air.transitions) {
			for (const inp of t.inputs) {
				expect(allPlaceIds.has(inp.place)).toBe(true);
			}
			for (const out of t.outputs) {
				expect(allPlaceIds.has(out.place)).toBe(true);
			}
		}

		// Both branch edges must produce pass-through transitions
		expect(air.transitions.find((t) => t.id === 't_edge_e-2')).toBeDefined();
		expect(air.transitions.find((t) => t.id === 't_edge_e-3')).toBeDefined();
	});
});

// ── AIR Structure Validation ──────────────────────────────────────────

describe('AIR output structure', () => {
	it('has required top-level fields', () => {
		const result = compileToAIR(startEndGraph(), 'Structure Test');
		const { air } = result;
		expect(air).toHaveProperty('name');
		expect(air).toHaveProperty('places');
		expect(air).toHaveProperty('transitions');
		expect(air).toHaveProperty('groups');
		expect(air).toHaveProperty('definitions');
		expect(Array.isArray(air.places)).toBe(true);
		expect(Array.isArray(air.transitions)).toBe(true);
		expect(Array.isArray(air.groups)).toBe(true);
	});

	it('places have correct structure', () => {
		const result = compileToAIR(startEndGraph(), 'Place Test');
		for (const place of result.air.places) {
			expect(place).toHaveProperty('id');
			expect(place).toHaveProperty('name');
			expect(place).toHaveProperty('type');
			expect(['state', 'resource', 'signal', 'terminal']).toContain(place.type);
		}
	});

	it('transitions have correct structure', () => {
		const result = compileToAIR(startEndGraph(), 'Transition Test');
		for (const t of result.air.transitions) {
			expect(t).toHaveProperty('id');
			expect(t).toHaveProperty('name');
			expect(t).toHaveProperty('input_ports');
			expect(t).toHaveProperty('output_ports');
			expect(t).toHaveProperty('inputs');
			expect(t).toHaveProperty('outputs');
			expect(t).toHaveProperty('logic');
			expect(Array.isArray(t.input_ports)).toBe(true);
			expect(Array.isArray(t.output_ports)).toBe(true);
			expect(Array.isArray(t.inputs)).toBe(true);
			expect(Array.isArray(t.outputs)).toBe(true);
		}
	});

	it('has exactly one place with initial_tokens (start)', () => {
		const result = compileToAIR(startEndGraph(), 'Token Test');
		const placesWithTokens = result.air.places.filter(
			(p) => p.initial_tokens && p.initial_tokens.length > 0
		);
		expect(placesWithTokens).toHaveLength(1);
	});

	it('has exactly one terminal place (end)', () => {
		const result = compileToAIR(startEndGraph(), 'Terminal Test');
		const terminalPlaces = result.air.places.filter((p) => p.type === 'terminal');
		expect(terminalPlaces).toHaveLength(1);
	});
});

// ── Reference fixture export ──────────────────────────────────────────

describe('AIR fixture export', () => {
	it('saves Start->End AIR fixture', () => {
		const result = compileToAIR(startEndGraph(), 'Start-End Fixture');
		expect(result.errors).toHaveLength(0);

		const fixtureDir = join(__dirname, '../../../tests/fixtures/air');
		mkdirSync(fixtureDir, { recursive: true });
		writeFileSync(
			join(fixtureDir, 'start-end.air.json'),
			JSON.stringify(result.air, null, 2) + '\n'
		);
	});

	it('saves Start->HumanTask->End AIR fixture', () => {
		const result = compileToAIR(startHumanTaskEndGraph(), 'HumanTask Fixture');
		expect(result.errors).toHaveLength(0);

		const fixtureDir = join(__dirname, '../../../tests/fixtures/air');
		mkdirSync(fixtureDir, { recursive: true });
		writeFileSync(
			join(fixtureDir, 'start-humantask-end.air.json'),
			JSON.stringify(result.air, null, 2) + '\n'
		);
	});
});
