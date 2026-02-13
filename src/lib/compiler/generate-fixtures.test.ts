import { describe, it } from 'vitest';
import { compileToAIR } from './compile';
import { writeFileSync, mkdirSync } from 'fs';
import { join } from 'path';
import {
	makeSimpleGraph,
	makeHumanTaskGraph,
	makeDecisionGraph,
	makeParallelGraph
} from './air-validation.test';

const fixturesDir = join(process.cwd(), 'tests/fixtures/air');

describe('Generate AIR fixtures', () => {
	it('generates simple-start-end.air.json', () => {
		mkdirSync(fixturesDir, { recursive: true });
		const { air } = compileToAIR(makeSimpleGraph(), 'Simple Start-End');
		writeFileSync(
			join(fixturesDir, 'simple-start-end.air.json'),
			JSON.stringify(air, null, 2) + '\n'
		);
	});

	it('generates linear-human-task.air.json', () => {
		const { air } = compileToAIR(makeHumanTaskGraph(), 'Linear Human Task');
		writeFileSync(
			join(fixturesDir, 'linear-human-task.air.json'),
			JSON.stringify(air, null, 2) + '\n'
		);
	});

	it('generates decision-two-branches.air.json', () => {
		const { air } = compileToAIR(makeDecisionGraph(), 'Decision Two Branches');
		writeFileSync(
			join(fixturesDir, 'decision-two-branches.air.json'),
			JSON.stringify(air, null, 2) + '\n'
		);
	});

	it('generates parallel-two-paths.air.json', () => {
		const { air } = compileToAIR(makeParallelGraph(), 'Parallel Two Paths');
		writeFileSync(
			join(fixturesDir, 'parallel-two-paths.air.json'),
			JSON.stringify(air, null, 2) + '\n'
		);
	});
});
