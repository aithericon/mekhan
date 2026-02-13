import { describe, it, expect } from 'vitest';
import { compileToAIR } from './compile';
import {
	makeSimpleGraph,
	makeHumanTaskGraph,
	makeAutomatedStepGraph,
	makeDecisionGraph,
	makeParallelGraph,
	makeLoopGraph
} from './air-validation.test';

describe('AIR node expansion: Start', () => {
	it('produces exactly 1 state place with initial_tokens', () => {
		const graph = makeSimpleGraph();
		const { air } = compileToAIR(graph, 'Test');
		const startPlaces = air.places.filter((p) => p.id.startsWith('p_start-1'));
		expect(startPlaces).toHaveLength(1);
		expect(startPlaces[0].type).toBe('state');
		expect(startPlaces[0].initial_tokens).toBeDefined();
		expect(startPlaces[0].initial_tokens!.length).toBeGreaterThan(0);
	});

	it('initial token contains __INSTANCE_ID__ placeholder', () => {
		const { air } = compileToAIR(makeSimpleGraph(), 'Test');
		const startPlace = air.places.find((p) => p.id === 'p_start-1_ready')!;
		expect(startPlace.initial_tokens![0]).toHaveProperty('_instance_id', '__INSTANCE_ID__');
	});
});

describe('AIR node expansion: End', () => {
	it('produces exactly 1 terminal place', () => {
		const { air } = compileToAIR(makeSimpleGraph(), 'Test');
		const endPlaces = air.places.filter((p) => p.id.startsWith('p_end-1'));
		expect(endPlaces).toHaveLength(1);
		expect(endPlaces[0].type).toBe('terminal');
	});

	it('terminal place has no initial_tokens', () => {
		const { air } = compileToAIR(makeSimpleGraph(), 'Test');
		const endPlace = air.places.find((p) => p.id === 'p_end-1_done')!;
		expect(endPlace.initial_tokens).toBeUndefined();
	});
});

describe('AIR node expansion: HumanTask', () => {
	it('produces 4 places: input, active, signal, output', () => {
		const { air } = compileToAIR(makeHumanTaskGraph(), 'Test');
		const htPlaces = air.places.filter((p) => p.id.startsWith('p_ht-1_'));
		expect(htPlaces).toHaveLength(4);

		const placeIds = htPlaces.map((p) => p.id);
		expect(placeIds).toContain('p_ht-1_input');
		expect(placeIds).toContain('p_ht-1_active');
		expect(placeIds).toContain('p_ht-1_signal');
		expect(placeIds).toContain('p_ht-1_output');
	});

	it('signal place has type "signal"', () => {
		const { air } = compileToAIR(makeHumanTaskGraph(), 'Test');
		const signal = air.places.find((p) => p.id === 'p_ht-1_signal')!;
		expect(signal.type).toBe('signal');
	});

	it('produces 2 transitions: request (effect:human_task) and finalize (guard)', () => {
		const { air } = compileToAIR(makeHumanTaskGraph(), 'Test');
		const htTransitions = air.transitions.filter((t) => t.id.startsWith('t_ht-1_'));
		expect(htTransitions).toHaveLength(2);

		const request = htTransitions.find((t) => t.id === 't_ht-1_request')!;
		expect(request).toBeDefined();
		expect(request.logic.type).toBe('effect');
		if (request.logic.type === 'effect') {
			expect(request.logic.handler_id).toBe('human_task');
		}

		const finalize = htTransitions.find((t) => t.id === 't_ht-1_finalize')!;
		expect(finalize).toBeDefined();
		expect(finalize.guard).toBeDefined();
		expect(finalize.guard!.type).toBe('rhai');
	});

	it('request transition reads from input, writes to active', () => {
		const { air } = compileToAIR(makeHumanTaskGraph(), 'Test');
		const request = air.transitions.find((t) => t.id === 't_ht-1_request')!;
		expect(request.inputs[0].place).toBe('p_ht-1_input');
		expect(request.outputs[0].place).toBe('p_ht-1_active');
	});

	it('finalize transition reads from active+signal, writes to output', () => {
		const { air } = compileToAIR(makeHumanTaskGraph(), 'Test');
		const finalize = air.transitions.find((t) => t.id === 't_ht-1_finalize')!;
		const inputPlaces = finalize.inputs.map((i) => i.place).sort();
		expect(inputPlaces).toEqual(['p_ht-1_active', 'p_ht-1_signal']);
		expect(finalize.outputs[0].place).toBe('p_ht-1_output');
	});

	it('creates a group for the human task', () => {
		const { air } = compileToAIR(makeHumanTaskGraph(), 'Test');
		const group = air.groups.find((g) => g.id === 'grp_ht-1');
		expect(group).toBeDefined();
		expect(group!.name).toBe('Review Task');
	});
});

describe('AIR node expansion: AutomatedStep', () => {
	it('produces 7 places: input, job, submitted, sig_complete, sig_failed, output, error', () => {
		const { air } = compileToAIR(makeAutomatedStepGraph(), 'Test');
		const autoPlaces = air.places.filter((p) => p.id.startsWith('p_auto-1_'));
		expect(autoPlaces).toHaveLength(7);

		const placeIds = autoPlaces.map((p) => p.id);
		expect(placeIds).toContain('p_auto-1_input');
		expect(placeIds).toContain('p_auto-1_job');
		expect(placeIds).toContain('p_auto-1_submitted');
		expect(placeIds).toContain('p_auto-1_sig_complete');
		expect(placeIds).toContain('p_auto-1_sig_failed');
		expect(placeIds).toContain('p_auto-1_output');
		expect(placeIds).toContain('p_auto-1_error');
	});

	it('has 2 signal places (complete and failed)', () => {
		const { air } = compileToAIR(makeAutomatedStepGraph(), 'Test');
		const signals = air.places.filter(
			(p) => p.id.startsWith('p_auto-1_sig_')
		);
		expect(signals).toHaveLength(2);
		expect(signals.every((s) => s.type === 'signal')).toBe(true);
	});

	it('produces 4 transitions: prepare, submit (effect), done, failed', () => {
		const { air } = compileToAIR(makeAutomatedStepGraph(), 'Test');
		const autoTransitions = air.transitions.filter((t) => t.id.startsWith('t_auto-1_'));
		expect(autoTransitions).toHaveLength(4);

		const ids = autoTransitions.map((t) => t.id);
		expect(ids).toContain('t_auto-1_prepare');
		expect(ids).toContain('t_auto-1_submit');
		expect(ids).toContain('t_auto-1_done');
		expect(ids).toContain('t_auto-1_failed');
	});

	it('submit transition has effect:executor_submit logic', () => {
		const { air } = compileToAIR(makeAutomatedStepGraph(), 'Test');
		const submit = air.transitions.find((t) => t.id === 't_auto-1_submit')!;
		expect(submit.logic.type).toBe('effect');
		if (submit.logic.type === 'effect') {
			expect(submit.logic.handler_id).toBe('executor_submit');
			expect(submit.logic.config).toHaveProperty('backend_type', 'python');
			expect(submit.logic.config).toHaveProperty('signal_complete', 'p_auto-1_sig_complete');
			expect(submit.logic.config).toHaveProperty('signal_failed', 'p_auto-1_sig_failed');
		}
	});

	it('done transition joins submitted + sig_complete', () => {
		const { air } = compileToAIR(makeAutomatedStepGraph(), 'Test');
		const done = air.transitions.find((t) => t.id === 't_auto-1_done')!;
		const inputPlaces = done.inputs.map((i) => i.place).sort();
		expect(inputPlaces).toEqual(['p_auto-1_sig_complete', 'p_auto-1_submitted']);
		expect(done.outputs[0].place).toBe('p_auto-1_output');
	});

	it('failed transition joins submitted + sig_failed', () => {
		const { air } = compileToAIR(makeAutomatedStepGraph(), 'Test');
		const failed = air.transitions.find((t) => t.id === 't_auto-1_failed')!;
		const inputPlaces = failed.inputs.map((i) => i.place).sort();
		expect(inputPlaces).toEqual(['p_auto-1_sig_failed', 'p_auto-1_submitted']);
		expect(failed.outputs[0].place).toBe('p_auto-1_error');
	});

	it('creates a group for the automated step', () => {
		const { air } = compileToAIR(makeAutomatedStepGraph(), 'Test');
		const group = air.groups.find((g) => g.id === 'grp_auto-1');
		expect(group).toBeDefined();
		expect(group!.name).toBe('Run Script');
	});
});

describe('AIR node expansion: Decision', () => {
	it('produces 1 input place + N output places', () => {
		const { air } = compileToAIR(makeDecisionGraph(), 'Test');
		const decPlaces = air.places.filter((p) => p.id.startsWith('p_dec-1_'));
		// 1 input + 2 branch outputs = 3
		expect(decPlaces).toHaveLength(3);

		expect(decPlaces.find((p) => p.id === 'p_dec-1_input')).toBeDefined();
		expect(decPlaces.find((p) => p.id === 'p_dec-1_out_branch-a')).toBeDefined();
		expect(decPlaces.find((p) => p.id === 'p_dec-1_out_branch-b')).toBeDefined();
	});

	it('produces N transitions with guards for each branch', () => {
		const { air } = compileToAIR(makeDecisionGraph(), 'Test');
		const branchTransitions = air.transitions.filter(
			(t) => t.id.startsWith('t_dec-1_branch_')
		);
		expect(branchTransitions).toHaveLength(2);

		const branchA = branchTransitions.find((t) => t.id === 't_dec-1_branch_branch-a')!;
		expect(branchA.guard).toBeDefined();
		expect(branchA.guard!.source).toBe('input.value > 10');

		const branchB = branchTransitions.find((t) => t.id === 't_dec-1_branch_branch-b')!;
		expect(branchB.guard).toBeDefined();
		expect(branchB.guard!.source).toBe('input.value <= 10');
	});

	it('all branch transitions read from the same input place', () => {
		const { air } = compileToAIR(makeDecisionGraph(), 'Test');
		const branchTransitions = air.transitions.filter(
			(t) => t.id.startsWith('t_dec-1_branch_')
		);
		for (const t of branchTransitions) {
			expect(t.inputs[0].place).toBe('p_dec-1_input');
		}
	});

	it('creates edge transitions for all edges including decision branches', () => {
		const { air } = compileToAIR(makeDecisionGraph(), 'Test');
		// All edges get wired in the unified edge wiring pass, including
		// edges sourced from decision nodes.
		const edgeTransitions = air.transitions.filter((t) => t.id.startsWith('t_edge_'));
		expect(edgeTransitions).toHaveLength(3); // e1 (start->decision), e-a (decision->end-a), e-b (decision->end-b)

		const e1 = edgeTransitions.find((t) => t.id === 't_edge_e1')!;
		expect(e1).toBeDefined();
		expect(e1.inputs[0].place).toBe('p_start-1_ready');
		expect(e1.outputs[0].place).toBe('p_dec-1_input');

		const eA = edgeTransitions.find((t) => t.id === 't_edge_e-a')!;
		expect(eA).toBeDefined();
		expect(eA.inputs[0].place).toBe('p_dec-1_out_branch-a');
		expect(eA.outputs[0].place).toBe('p_end-a_done');

		const eB = edgeTransitions.find((t) => t.id === 't_edge_e-b')!;
		expect(eB).toBeDefined();
		expect(eB.inputs[0].place).toBe('p_dec-1_out_branch-b');
		expect(eB.outputs[0].place).toBe('p_end-b_done');
	});
});

describe('AIR node expansion: ParallelSplit', () => {
	it('produces 1 input place + N output places', () => {
		const { air } = compileToAIR(makeParallelGraph(), 'Test');
		const splitPlaces = air.places.filter((p) => p.id.startsWith('p_split-1_'));
		// 1 input + 2 outputs = 3
		expect(splitPlaces).toHaveLength(3);
		expect(splitPlaces.find((p) => p.id === 'p_split-1_input')).toBeDefined();
		expect(splitPlaces.find((p) => p.id === 'p_split-1_out_0')).toBeDefined();
		expect(splitPlaces.find((p) => p.id === 'p_split-1_out_1')).toBeDefined();
	});

	it('produces a fork transition with 1 input port and N output ports', () => {
		const { air } = compileToAIR(makeParallelGraph(), 'Test');
		const fork = air.transitions.find((t) => t.id === 't_split-1_fork')!;
		expect(fork).toBeDefined();
		expect(fork.input_ports).toHaveLength(1);
		expect(fork.output_ports).toHaveLength(2);
		expect(fork.inputs).toHaveLength(1);
		expect(fork.outputs).toHaveLength(2);
	});

	it('fork transition input reads from the split input place', () => {
		const { air } = compileToAIR(makeParallelGraph(), 'Test');
		const fork = air.transitions.find((t) => t.id === 't_split-1_fork')!;
		expect(fork.inputs[0].place).toBe('p_split-1_input');
	});
});

describe('AIR node expansion: ParallelJoin', () => {
	it('produces N input places + 1 output place', () => {
		const { air } = compileToAIR(makeParallelGraph(), 'Test');
		const joinPlaces = air.places.filter((p) => p.id.startsWith('p_join-1_'));
		// 2 inputs + 1 output = 3
		expect(joinPlaces).toHaveLength(3);
		expect(joinPlaces.find((p) => p.id === 'p_join-1_in_0')).toBeDefined();
		expect(joinPlaces.find((p) => p.id === 'p_join-1_in_1')).toBeDefined();
		expect(joinPlaces.find((p) => p.id === 'p_join-1_output')).toBeDefined();
	});

	it('produces a join transition with N input ports and 1 output port', () => {
		const { air } = compileToAIR(makeParallelGraph(), 'Test');
		const join = air.transitions.find((t) => t.id === 't_join-1_join')!;
		expect(join).toBeDefined();
		expect(join.input_ports).toHaveLength(2);
		expect(join.output_ports).toHaveLength(1);
		expect(join.inputs).toHaveLength(2);
		expect(join.outputs).toHaveLength(1);
	});

	it('join transition output writes to the join output place', () => {
		const { air } = compileToAIR(makeParallelGraph(), 'Test');
		const join = air.transitions.find((t) => t.id === 't_join-1_join')!;
		expect(join.outputs[0].place).toBe('p_join-1_output');
	});
});

describe('AIR node expansion: Loop', () => {
	it('produces 4 places: input, body_in, body_out, output', () => {
		const { air } = compileToAIR(makeLoopGraph(), 'Test');
		const loopPlaces = air.places.filter((p) => p.id.startsWith('p_loop-1_'));
		expect(loopPlaces).toHaveLength(4);

		const placeIds = loopPlaces.map((p) => p.id);
		expect(placeIds).toContain('p_loop-1_input');
		expect(placeIds).toContain('p_loop-1_body_in');
		expect(placeIds).toContain('p_loop-1_body_out');
		expect(placeIds).toContain('p_loop-1_output');
	});

	it('produces 3 transitions: enter, continue (with guard), exit (with guard)', () => {
		const { air } = compileToAIR(makeLoopGraph(), 'Test');
		const loopTransitions = air.transitions.filter((t) => t.id.startsWith('t_loop-1_'));
		expect(loopTransitions).toHaveLength(3);

		const enter = loopTransitions.find((t) => t.id === 't_loop-1_enter')!;
		expect(enter).toBeDefined();
		expect(enter.guard).toBeUndefined();

		const cont = loopTransitions.find((t) => t.id === 't_loop-1_continue')!;
		expect(cont).toBeDefined();
		expect(cont.guard).toBeDefined();
		expect(cont.guard!.source).toContain('< 3');
		expect(cont.guard!.source).toContain('input.retry == true');

		const exit = loopTransitions.find((t) => t.id === 't_loop-1_exit')!;
		expect(exit).toBeDefined();
		expect(exit.guard).toBeDefined();
		expect(exit.guard!.source).toContain('>= 3');
	});

	it('enter transition: input -> body_in with counter init', () => {
		const { air } = compileToAIR(makeLoopGraph(), 'Test');
		const enter = air.transitions.find((t) => t.id === 't_loop-1_enter')!;
		expect(enter.inputs[0].place).toBe('p_loop-1_input');
		expect(enter.outputs[0].place).toBe('p_loop-1_body_in');
		if (enter.logic.type === 'rhai') {
			expect(enter.logic.source).toContain('_loop_loop-1_count');
			expect(enter.logic.source).toContain('= 0');
		}
	});

	it('continue transition: body_out -> body_in (loop back)', () => {
		const { air } = compileToAIR(makeLoopGraph(), 'Test');
		const cont = air.transitions.find((t) => t.id === 't_loop-1_continue')!;
		expect(cont.inputs[0].place).toBe('p_loop-1_body_out');
		expect(cont.outputs[0].place).toBe('p_loop-1_body_in');
	});

	it('exit transition: body_out -> output', () => {
		const { air } = compileToAIR(makeLoopGraph(), 'Test');
		const exit = air.transitions.find((t) => t.id === 't_loop-1_exit')!;
		expect(exit.inputs[0].place).toBe('p_loop-1_body_out');
		expect(exit.outputs[0].place).toBe('p_loop-1_output');
	});
});

describe('AIR edge wiring', () => {
	it('creates pass-through transitions for sequence edges', () => {
		const { air } = compileToAIR(makeSimpleGraph(), 'Test');
		// Start -> End should have 1 edge transition
		const edgeTransitions = air.transitions.filter((t) => t.id.startsWith('t_edge_'));
		expect(edgeTransitions).toHaveLength(1);
		expect(edgeTransitions[0].inputs[0].place).toBe('p_start-1_ready');
		expect(edgeTransitions[0].outputs[0].place).toBe('p_end-1_done');
	});

	it('pass-through transitions have identity logic', () => {
		const { air } = compileToAIR(makeSimpleGraph(), 'Test');
		const edgeT = air.transitions.find((t) => t.id.startsWith('t_edge_'))!;
		expect(edgeT.logic.type).toBe('rhai');
		if (edgeT.logic.type === 'rhai') {
			expect(edgeT.logic.source).toBe('#{ output: input }');
		}
	});

	it('does not create duplicate edge transitions for decision branches', () => {
		const { air } = compileToAIR(makeDecisionGraph(), 'Test');
		// Decision handles its own wiring; the main loop should skip decision source edges
		// We should have edge transitions from decision's internal wiring but NOT duplicated
		const edgeIds = air.transitions.filter((t) => t.id.startsWith('t_edge_')).map((t) => t.id);
		const uniqueIds = new Set(edgeIds);
		expect(uniqueIds.size).toBe(edgeIds.length);
	});
});
