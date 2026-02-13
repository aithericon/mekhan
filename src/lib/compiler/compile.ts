import type {
	WorkflowGraph,
	WorkflowNodeData,
	HumanTaskNodeData,
	AutomatedStepNodeData,
	DecisionNodeData,
	LoopNodeData
} from '$lib/types/editor';
import { validateGraph, type ValidationError } from './validate';

/** AIR format types */
type AIRPlace = {
	id: string;
	name: string;
	type: 'state' | 'resource' | 'signal' | 'terminal';
	group_id?: string;
	initial_tokens?: unknown[];
	token_schema?: string;
};

type AIRPort = {
	name: string;
	schema_ref?: string;
	cardinality: 'single' | 'batch';
};

type AIRArc = {
	place: string;
	port: string;
	weight?: number;
};

type AIRLogic =
	| { type: 'rhai'; source: string }
	| { type: 'effect'; handler_id: string; config: Record<string, unknown> };

type AIRTransition = {
	id: string;
	name: string;
	group_id?: string;
	input_ports: AIRPort[];
	output_ports: AIRPort[];
	inputs: AIRArc[];
	outputs: AIRArc[];
	guard?: { type: 'rhai'; source: string };
	logic: AIRLogic;
};

type AIRGroup = {
	id: string;
	name: string;
	parent_id?: string;
};

type AIRDocument = {
	name: string;
	description?: string;
	places: AIRPlace[];
	transitions: AIRTransition[];
	groups: AIRGroup[];
	definitions: Record<string, unknown>;
};

export type CompileOutput = {
	air: AIRDocument;
	errors: ValidationError[];
	warnings: string[];
};

/**
 * Compile a WorkflowGraph to AIR JSON.
 * This is the client-side compiler for instant preview.
 */
export function compileToAIR(
	graph: WorkflowGraph,
	name: string,
	description?: string
): CompileOutput {
	const errors = validateGraph(graph);
	const warnings: string[] = [];

	if (errors.length > 0) {
		return {
			air: { name, places: [], transitions: [], groups: [], definitions: {} },
			errors,
			warnings
		};
	}

	const places: AIRPlace[] = [];
	const transitions: AIRTransition[] = [];
	const groups: AIRGroup[] = [];

	// Track input/output places for each node (for wiring)
	const nodeInputPlace = new Map<string, string>();
	const nodeOutputPlace = new Map<string, string>();
	// For decision/parallel_split: maps "nodeId:edgeIdentifier" -> output place
	const nodeOutputPlaceByEdge = new Map<string, string>();

	// Build adjacency
	const outgoing = new Map<string, string[]>();
	for (const edge of graph.edges) {
		if (!outgoing.has(edge.source)) outgoing.set(edge.source, []);
		outgoing.get(edge.source)!.push(edge.target);
	}

	// Step 1: Expand each node to places/transitions
	for (const node of graph.nodes) {
		switch (node.data.type) {
			case 'start':
				expandStart(node.id, node.data, places, nodeInputPlace, nodeOutputPlace);
				break;
			case 'end':
				expandEnd(node.id, node.data, places, nodeInputPlace, nodeOutputPlace);
				break;
			case 'human_task':
				expandHumanTask(node.id, node.data, places, transitions, groups, nodeInputPlace, nodeOutputPlace);
				break;
			case 'automated_step':
				expandAutomatedStep(node.id, node.data, places, transitions, groups, nodeInputPlace, nodeOutputPlace);
				break;
			case 'decision':
				expandDecision(node.id, node.data, graph, places, transitions, nodeInputPlace, nodeOutputPlace, nodeOutputPlaceByEdge);
				break;
			case 'parallel_split':
				expandParallelSplit(node.id, node.data, graph, places, transitions, nodeInputPlace, nodeOutputPlace, nodeOutputPlaceByEdge);
				break;
			case 'parallel_join':
				expandParallelJoin(node.id, node.data, graph, places, transitions, nodeInputPlace, nodeOutputPlace);
				break;
			case 'loop':
				expandLoop(node.id, node.data, places, transitions, nodeInputPlace, nodeOutputPlace);
				break;
		}
	}

	// Step 2: Wire ALL edges (create pass-through transitions)
	for (const edge of graph.edges) {
		const sourceNode = graph.nodes.find((n) => n.id === edge.source);
		const targetNode = graph.nodes.find((n) => n.id === edge.target);
		if (!sourceNode || !targetNode) continue;

		// Determine the output place for this edge's source
		let outputPlace: string | undefined;

		if (sourceNode.data.type === 'decision') {
			// For decision nodes: look up the output place by sourceHandle (condition edgeId)
			if (edge.sourceHandle) {
				outputPlace = nodeOutputPlaceByEdge.get(`${edge.source}:${edge.sourceHandle}`);
			}
			// If no sourceHandle match, try the default branch
			if (!outputPlace) {
				outputPlace = nodeOutputPlaceByEdge.get(`${edge.source}:default`);
			}
		} else if (sourceNode.data.type === 'parallel_split') {
			// For parallel_split nodes: look up the output place by edge id
			outputPlace = nodeOutputPlaceByEdge.get(`${edge.source}:${edge.id}`);
		} else {
			outputPlace = nodeOutputPlace.get(edge.source);
		}

		const inputPlace = nodeInputPlace.get(edge.target);
		if (!outputPlace || !inputPlace) continue;

		// Create pass-through transition
		transitions.push({
			id: `t_edge_${edge.id}`,
			name: `${sourceNode.data.label} -> ${targetNode.data.label}`,
			input_ports: [{ name: 'input', cardinality: 'single' }],
			output_ports: [{ name: 'output', cardinality: 'single' }],
			inputs: [{ place: outputPlace, port: 'input' }],
			outputs: [{ port: 'output', place: inputPlace }],
			logic: { type: 'rhai', source: '#{ output: input }' }
		});
	}

	return {
		air: { name, description, places, transitions, groups, definitions: {} },
		errors: [],
		warnings
	};
}

function expandStart(
	id: string,
	data: WorkflowNodeData,
	places: AIRPlace[],
	nodeInput: Map<string, string>,
	nodeOutput: Map<string, string>
) {
	const placeId = `p_${id}_ready`;
	places.push({
		id: placeId,
		name: data.label,
		type: 'state',
		initial_tokens: [{ _instance_id: '__INSTANCE_ID__', _created_at: '__TIMESTAMP__' }]
	});
	nodeInput.set(id, placeId);
	nodeOutput.set(id, placeId);
}

function expandEnd(
	id: string,
	data: WorkflowNodeData,
	places: AIRPlace[],
	nodeInput: Map<string, string>,
	nodeOutput: Map<string, string>
) {
	const placeId = `p_${id}_done`;
	places.push({
		id: placeId,
		name: data.label,
		type: 'terminal'
	});
	nodeInput.set(id, placeId);
	nodeOutput.set(id, placeId);
}

function expandHumanTask(
	id: string,
	data: HumanTaskNodeData,
	places: AIRPlace[],
	transitions: AIRTransition[],
	groups: AIRGroup[],
	nodeInput: Map<string, string>,
	nodeOutput: Map<string, string>
) {
	const inputPlace = `p_${id}_input`;
	const activePlace = `p_${id}_active`;
	const signalPlace = `p_${id}_signal`;
	const outputPlace = `p_${id}_output`;

	places.push(
		{ id: inputPlace, name: `${data.label} - Input`, type: 'state', group_id: `grp_${id}` },
		{ id: activePlace, name: `${data.label} - Active`, type: 'state', group_id: `grp_${id}` },
		{ id: signalPlace, name: `${data.label} - Signal`, type: 'signal', group_id: `grp_${id}` },
		{ id: outputPlace, name: `${data.label} - Output`, type: 'state', group_id: `grp_${id}` }
	);

	groups.push({ id: `grp_${id}`, name: data.label });

	// Serialize the task form definition into Rhai logic
	const stepsJson = JSON.stringify(
		data.steps.map((step) => ({
			id: step.id,
			title: step.title,
			description_mdsvex: step.descriptionMdsvex,
			blocks: step.blocks.map((block) => {
				if (block.type === 'input') {
					return {
						type: 'input',
						field: {
							name: block.field.name,
							label: block.field.label,
							kind: block.field.kind,
							required: block.field.required ?? false,
							placeholder: block.field.placeholder,
							options: block.field.options
						}
					};
				}
				return block;
			})
		}))
	).replace(/"/g, '\\"');

	// Request transition (effect: human_task)
	transitions.push({
		id: `t_${id}_request`,
		name: `${data.label} - Request`,
		group_id: `grp_${id}`,
		input_ports: [{ name: 'task', cardinality: 'single' }],
		output_ports: [{ name: 'assigned', cardinality: 'single' }],
		inputs: [{ place: inputPlace, port: 'task' }],
		outputs: [{ port: 'assigned', place: activePlace }],
		logic: {
			type: 'effect',
			handler_id: 'human_task',
			config: { place: signalPlace }
		}
	});

	// Collect field names for merge logic
	const fieldNames: string[] = [];
	for (const step of data.steps) {
		for (const block of step.blocks) {
			if (block.type === 'input') {
				fieldNames.push(block.field.name);
			}
		}
	}

	const mergeFields = fieldNames
		.map((f) => `${f}: signal.${f}`)
		.join(', ');

	// Finalize transition (join active + signal)
	transitions.push({
		id: `t_${id}_finalize`,
		name: `${data.label} - Finalize`,
		group_id: `grp_${id}`,
		input_ports: [
			{ name: 'state', cardinality: 'single' },
			{ name: 'signal', cardinality: 'single' }
		],
		output_ports: [{ name: 'done', cardinality: 'single' }],
		inputs: [
			{ place: activePlace, port: 'state' },
			{ place: signalPlace, port: 'signal' }
		],
		outputs: [{ port: 'done', place: outputPlace }],
		guard: { type: 'rhai', source: 'signal.task_id == state.task_id' },
		logic: {
			type: 'rhai',
			source: mergeFields
				? `let d = state; ${fieldNames.map((f) => `d.${f} = signal.${f};`).join(' ')} #{ done: d }`
				: '#{ done: state }'
		}
	});

	nodeInput.set(id, inputPlace);
	nodeOutput.set(id, outputPlace);
}

function expandAutomatedStep(
	id: string,
	data: AutomatedStepNodeData,
	places: AIRPlace[],
	transitions: AIRTransition[],
	groups: AIRGroup[],
	nodeInput: Map<string, string>,
	nodeOutput: Map<string, string>
) {
	const inputPlace = `p_${id}_input`;
	const jobPlace = `p_${id}_job`;
	const submittedPlace = `p_${id}_submitted`;
	const sigComplete = `p_${id}_sig_complete`;
	const sigFailed = `p_${id}_sig_failed`;
	const outputPlace = `p_${id}_output`;
	const errorPlace = `p_${id}_error`;

	places.push(
		{ id: inputPlace, name: `${data.label} - Input`, type: 'state', group_id: `grp_${id}` },
		{ id: jobPlace, name: `${data.label} - Job`, type: 'state', group_id: `grp_${id}` },
		{ id: submittedPlace, name: `${data.label} - Submitted`, type: 'state', group_id: `grp_${id}` },
		{ id: sigComplete, name: `${data.label} - Complete Signal`, type: 'signal', group_id: `grp_${id}` },
		{ id: sigFailed, name: `${data.label} - Failed Signal`, type: 'signal', group_id: `grp_${id}` },
		{ id: outputPlace, name: `${data.label} - Output`, type: 'state', group_id: `grp_${id}` },
		{ id: errorPlace, name: `${data.label} - Error`, type: 'state', group_id: `grp_${id}` }
	);

	groups.push({ id: `grp_${id}`, name: data.label });

	transitions.push(
		{
			id: `t_${id}_prepare`,
			name: `${data.label} - Prepare`,
			group_id: `grp_${id}`,
			input_ports: [{ name: 'input', cardinality: 'single' }],
			output_ports: [{ name: 'job', cardinality: 'single' }],
			inputs: [{ place: inputPlace, port: 'input' }],
			outputs: [{ port: 'job', place: jobPlace }],
			logic: { type: 'rhai', source: '#{ job: input }' }
		},
		{
			id: `t_${id}_submit`,
			name: `${data.label} - Submit`,
			group_id: `grp_${id}`,
			input_ports: [{ name: 'job', cardinality: 'single' }],
			output_ports: [{ name: 'submitted', cardinality: 'single' }],
			inputs: [{ place: jobPlace, port: 'job' }],
			outputs: [{ port: 'submitted', place: submittedPlace }],
			logic: {
				type: 'effect',
				handler_id: 'executor_submit',
				config: {
					backend_type: data.executionSpec.backendType,
					signal_complete: sigComplete,
					signal_failed: sigFailed,
					...data.executionSpec.config
				}
			}
		},
		{
			id: `t_${id}_done`,
			name: `${data.label} - Done`,
			group_id: `grp_${id}`,
			input_ports: [
				{ name: 'state', cardinality: 'single' },
				{ name: 'signal', cardinality: 'single' }
			],
			output_ports: [{ name: 'output', cardinality: 'single' }],
			inputs: [
				{ place: submittedPlace, port: 'state' },
				{ place: sigComplete, port: 'signal' }
			],
			outputs: [{ port: 'output', place: outputPlace }],
			logic: { type: 'rhai', source: 'let d = state; d.result = signal; #{ output: d }' }
		},
		{
			id: `t_${id}_failed`,
			name: `${data.label} - Failed`,
			group_id: `grp_${id}`,
			input_ports: [
				{ name: 'state', cardinality: 'single' },
				{ name: 'signal', cardinality: 'single' }
			],
			output_ports: [{ name: 'error', cardinality: 'single' }],
			inputs: [
				{ place: submittedPlace, port: 'state' },
				{ place: sigFailed, port: 'signal' }
			],
			outputs: [{ port: 'error', place: errorPlace }],
			logic: { type: 'rhai', source: 'let d = state; d.error = signal; #{ error: d }' }
		}
	);

	nodeInput.set(id, inputPlace);
	nodeOutput.set(id, outputPlace);
}

function expandDecision(
	id: string,
	data: DecisionNodeData,
	graph: WorkflowGraph,
	places: AIRPlace[],
	transitions: AIRTransition[],
	nodeInput: Map<string, string>,
	nodeOutput: Map<string, string>,
	nodeOutputByEdge: Map<string, string>
) {
	const inputPlace = `p_${id}_input`;
	places.push({ id: inputPlace, name: `${data.label} - Input`, type: 'state' });
	nodeInput.set(id, inputPlace);

	// Create a guarded branch transition + output place for each condition
	for (const condition of data.conditions) {
		const outPlace = `p_${id}_out_${condition.edgeId}`;
		places.push({ id: outPlace, name: `${data.label} - ${condition.label}`, type: 'state' });

		transitions.push({
			id: `t_${id}_branch_${condition.edgeId}`,
			name: `${data.label} - ${condition.label}`,
			input_ports: [{ name: 'input', cardinality: 'single' }],
			output_ports: [{ name: 'output', cardinality: 'single' }],
			inputs: [{ place: inputPlace, port: 'input' }],
			outputs: [{ port: 'output', place: outPlace }],
			guard: condition.guard ? { type: 'rhai', source: condition.guard } : undefined,
			logic: { type: 'rhai', source: '#{ output: input }' }
		});

		// Register this output place keyed by the condition's edgeId (matches edge.sourceHandle)
		nodeOutputByEdge.set(`${id}:${condition.edgeId}`, outPlace);
	}

	// Default branch (no guard) — find outgoing edges not covered by any condition
	const outEdges = graph.edges.filter((e) => e.source === id);
	const defaultEdge = outEdges.find(
		(e) => !data.conditions.some((c) => c.edgeId === e.sourceHandle)
	);
	if (defaultEdge) {
		const outPlace = `p_${id}_out_default`;
		places.push({ id: outPlace, name: `${data.label} - Default`, type: 'state' });

		transitions.push({
			id: `t_${id}_default`,
			name: `${data.label} - Default`,
			input_ports: [{ name: 'input', cardinality: 'single' }],
			output_ports: [{ name: 'output', cardinality: 'single' }],
			inputs: [{ place: inputPlace, port: 'input' }],
			outputs: [{ port: 'output', place: outPlace }],
			logic: { type: 'rhai', source: '#{ output: input }' }
		});

		// Register the default output using the edge's sourceHandle if present, or 'default'
		const key = defaultEdge.sourceHandle ?? 'default';
		nodeOutputByEdge.set(`${id}:${key}`, outPlace);
	}

	// Decision doesn't have a single output place
	nodeOutput.set(id, `p_${id}_out_default`);
}

function expandParallelSplit(
	id: string,
	data: WorkflowNodeData,
	graph: WorkflowGraph,
	places: AIRPlace[],
	transitions: AIRTransition[],
	nodeInput: Map<string, string>,
	nodeOutput: Map<string, string>,
	nodeOutputByEdge: Map<string, string>
) {
	const inputPlace = `p_${id}_input`;
	places.push({ id: inputPlace, name: `${data.label} - Input`, type: 'state' });
	nodeInput.set(id, inputPlace);

	const outEdges = graph.edges.filter((e) => e.source === id);
	const outputPorts: AIRPort[] = [];
	const outputs: AIRArc[] = [];

	for (let i = 0; i < outEdges.length; i++) {
		const edge = outEdges[i];
		const outPlace = `p_${id}_out_${i}`;
		places.push({ id: outPlace, name: `${data.label} - Branch ${i}`, type: 'state' });

		const portName = `out_${i}`;
		outputPorts.push({ name: portName, cardinality: 'single' });
		outputs.push({ port: portName, place: outPlace });

		// Register this output place keyed by edge id for the wiring pass
		nodeOutputByEdge.set(`${id}:${edge.id}`, outPlace);
	}

	// Fork transition: one input, N outputs
	const forkLogic = outputPorts.map((p) => `${p.name}: input`).join(', ');
	transitions.push({
		id: `t_${id}_fork`,
		name: `${data.label} - Fork`,
		input_ports: [{ name: 'input', cardinality: 'single' }],
		output_ports: outputPorts,
		inputs: [{ place: inputPlace, port: 'input' }],
		outputs,
		logic: { type: 'rhai', source: `#{ ${forkLogic} }` }
	});

	nodeOutput.set(id, inputPlace); // Placeholder
}

function expandParallelJoin(
	id: string,
	data: WorkflowNodeData,
	graph: WorkflowGraph,
	places: AIRPlace[],
	transitions: AIRTransition[],
	nodeInput: Map<string, string>,
	nodeOutput: Map<string, string>
) {
	const outputPlace = `p_${id}_output`;
	places.push({ id: outputPlace, name: `${data.label} - Output`, type: 'state' });
	nodeOutput.set(id, outputPlace);

	const inEdges = graph.edges.filter((e) => e.target === id);
	const inputPorts: AIRPort[] = [];
	const inputs: AIRArc[] = [];

	for (let i = 0; i < inEdges.length; i++) {
		const inPlace = `p_${id}_in_${i}`;
		places.push({ id: inPlace, name: `${data.label} - Input ${i}`, type: 'state' });
		nodeInput.set(`${id}_${i}`, inPlace);

		const portName = `in_${i}`;
		inputPorts.push({ name: portName, cardinality: 'single' });
		inputs.push({ place: inPlace, port: portName });
	}

	// Set the first input place as the main input for edge wiring
	if (inEdges.length > 0) {
		nodeInput.set(id, `p_${id}_in_0`);
	}

	// Join transition: N inputs, one output
	const mergeLogic = inputPorts.map((p) => `d.${p.name} = ${p.name};`).join(' ');
	transitions.push({
		id: `t_${id}_join`,
		name: `${data.label} - Join`,
		input_ports: inputPorts,
		output_ports: [{ name: 'output', cardinality: 'single' }],
		inputs,
		outputs: [{ port: 'output', place: outputPlace }],
		logic: {
			type: 'rhai',
			source: inputPorts.length > 0
				? `let d = #{};  ${mergeLogic} #{ output: d }`
				: '#{ output: #{} }'
		}
	});
}

function expandLoop(
	id: string,
	data: LoopNodeData,
	places: AIRPlace[],
	transitions: AIRTransition[],
	nodeInput: Map<string, string>,
	nodeOutput: Map<string, string>
) {
	const inputPlace = `p_${id}_input`;
	const bodyIn = `p_${id}_body_in`;
	const bodyOut = `p_${id}_body_out`;
	const outputPlace = `p_${id}_output`;
	const counterKey = `_loop_${id}_count`;

	places.push(
		{ id: inputPlace, name: `${data.label} - Input`, type: 'state' },
		{ id: bodyIn, name: `${data.label} - Body In`, type: 'state' },
		{ id: bodyOut, name: `${data.label} - Body Out`, type: 'state' },
		{ id: outputPlace, name: `${data.label} - Output`, type: 'state' }
	);

	// Enter transition: initialize counter
	transitions.push({
		id: `t_${id}_enter`,
		name: `${data.label} - Enter`,
		input_ports: [{ name: 'input', cardinality: 'single' }],
		output_ports: [{ name: 'body', cardinality: 'single' }],
		inputs: [{ place: inputPlace, port: 'input' }],
		outputs: [{ port: 'body', place: bodyIn }],
		logic: {
			type: 'rhai',
			source: `let d = input; d.${counterKey} = 0; #{ body: d }`
		}
	});

	// Continue transition: loop back
	transitions.push({
		id: `t_${id}_continue`,
		name: `${data.label} - Continue`,
		input_ports: [{ name: 'input', cardinality: 'single' }],
		output_ports: [{ name: 'body', cardinality: 'single' }],
		inputs: [{ place: bodyOut, port: 'input' }],
		outputs: [{ port: 'body', place: bodyIn }],
		guard: {
			type: 'rhai',
			source: `input.${counterKey} < ${data.maxIterations} && (${data.loopCondition})`
		},
		logic: {
			type: 'rhai',
			source: `let d = input; d.${counterKey} = d.${counterKey} + 1; #{ body: d }`
		}
	});

	// Exit transition: leave loop
	transitions.push({
		id: `t_${id}_exit`,
		name: `${data.label} - Exit`,
		input_ports: [{ name: 'input', cardinality: 'single' }],
		output_ports: [{ name: 'output', cardinality: 'single' }],
		inputs: [{ place: bodyOut, port: 'input' }],
		outputs: [{ port: 'output', place: outputPlace }],
		guard: {
			type: 'rhai',
			source: `input.${counterKey} >= ${data.maxIterations} || !(${data.loopCondition})`
		},
		logic: { type: 'rhai', source: '#{ output: input }' }
	});

	nodeInput.set(id, inputPlace);
	nodeOutput.set(id, outputPlace);
}
