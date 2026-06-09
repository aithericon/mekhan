/**
 * Component (DOM) tests for the nested caps editor.
 *
 * Real `@testing-library/svelte` renders, enabled by `resolve.conditions:
 * ['browser']` in `vitest.config.ts` (same as `SchemaBuilder.test.ts`). The
 * capability registry the editor loads on mount is mocked so the typed-input
 * branching and the value↔onchange round-trip are deterministic and offline.
 *
 * The caps bag is TWO-LEVEL — `{ "<capability>": { "<field>": value } }` — so
 * what these lock in:
 *   - adding a capability emits the MERGED bag (existing caps preserved + the new
 *     capability seeded with its declared fields' defaults, Select → first option),
 *   - removing a capability drops just that capability key,
 *   - editing a typed field re-emits the bag with the parsed (typed) field value,
 *   - a field-less capability type renders the presence hint (no field inputs).
 */
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup } from '@testing-library/svelte';

// Mock the registry loader BEFORE importing the component (its onMount calls it).
// Two types: `lab_skill` (typed fields) and `docker` (presence — no fields).
vi.mock('$lib/api/capability-types', () => ({
	listCapabilityTypes: vi.fn(async () => ({
		items: [
			{
				id: 'ct-1',
				name: 'lab_skill',
				created_at: '2026-06-09T00:00:00Z',
				fields: [
					{ name: 'shift', kind: 'select', options: ['day', 'night'], required: false },
					{ name: 'pipettes', kind: 'bool', required: false },
					{ name: 'years', kind: 'number', required: false },
					{ name: 'badge', kind: 'text', required: false }
				]
			},
			{
				id: 'ct-2',
				name: 'docker',
				created_at: '2026-06-09T00:00:00Z',
				fields: []
			}
		],
		page: 1,
		per_page: 200,
		total: 2
	}))
}));

import CapsEditor from './CapsEditor.svelte';

afterEach(() => cleanup());

type Emitted = Record<string, unknown>;

function setup(value: Record<string, unknown>) {
	const onchange = vi.fn();
	const utils = render(CapsEditor, { props: { value, onchange } });
	const lastEmitted = (): Emitted | undefined => {
		const calls = onchange.mock.calls;
		return calls.length ? (calls[calls.length - 1][0] as Emitted) : undefined;
	};
	return { onchange, lastEmitted, ...utils };
}

// `listCapabilityTypes` is async (onMount → .then), so let the microtask that
// resolves it flush before asserting on registry-driven rendering.
async function flush() {
	await Promise.resolve();
	await Promise.resolve();
}

describe('empty bag', () => {
	it('renders the empty-state hint and no rows', () => {
		const { getByText } = setup({});
		expect(getByText(/No capabilities assigned/i)).toBeTruthy();
	});
});

describe('add capability', () => {
	it('seeds the first unused type with its declared field defaults', async () => {
		const { getByText, lastEmitted } = setup({});
		await flush();
		await fireEvent.click(getByText('Add capability'));

		const emitted = lastEmitted();
		expect(emitted).toBeDefined();
		// First unused type is `lab_skill`; its fields seed to kind defaults
		// (select → first option, bool → false, number → 0, text → '').
		expect(emitted).toHaveProperty('lab_skill');
		expect(emitted!.lab_skill).toEqual({ shift: 'day', pipettes: false, years: 0, badge: '' });
	});

	it('emits a merged bag preserving existing capabilities', async () => {
		const { getByText, lastEmitted } = setup({ docker: {} });
		await flush();
		await fireEvent.click(getByText('Add capability'));

		const emitted = lastEmitted();
		expect(emitted).toBeDefined();
		// Existing presence capability preserved…
		expect(emitted!.docker).toEqual({});
		// …plus the newly seeded typed capability.
		expect(emitted).toHaveProperty('lab_skill');
		expect(Object.keys(emitted!).length).toBe(2);
	});
});

describe('remove capability', () => {
	it('drops just that capability from the emitted bag', async () => {
		const { getAllByText, lastEmitted } = setup({ docker: {}, lab_skill: { years: 3 } });
		await flush();
		// Two capabilities → two Remove buttons; remove the first (`docker`).
		const removes = getAllByText('Remove');
		expect(removes.length).toBe(2);
		await fireEvent.click(removes[0]);

		const emitted = lastEmitted();
		expect(emitted).toBeDefined();
		expect(emitted).not.toHaveProperty('docker');
		expect(emitted!.lab_skill).toEqual({ years: 3 });
	});
});

describe('typed field round-trip', () => {
	it('parses a number field to a number and nests it under the capability', async () => {
		const { getByLabelText, lastEmitted } = setup({ lab_skill: { years: 0 } });
		await flush();
		// The number field's input is labelled by its field name `years`.
		const input = getByLabelText('years') as HTMLInputElement;
		input.value = '7';
		await fireEvent.input(input);

		const emitted = lastEmitted();
		expect(emitted).toBeDefined();
		expect((emitted!.lab_skill as Record<string, unknown>).years).toBe(7); // number, not "7"
	});

	it('keeps a text field value as a string', async () => {
		const { getByLabelText, lastEmitted } = setup({ lab_skill: { badge: '' } });
		await flush();
		const input = getByLabelText('badge') as HTMLInputElement;
		input.value = 'B-22';
		await fireEvent.input(input);

		const emitted = lastEmitted();
		expect(emitted).toBeDefined();
		expect((emitted!.lab_skill as Record<string, unknown>).badge).toBe('B-22');
	});
});

describe('presence capability', () => {
	it('renders the no-fields hint for a field-less type', async () => {
		const { getByText } = setup({ docker: {} });
		await flush();
		expect(getByText(/Presence capability — no fields/i)).toBeTruthy();
	});
});
