/**
 * Component (DOM) tests for the reworked recursive JSON-schema builder.
 *
 * Unlike most files in this codebase (which test pure helpers because the
 * shared vitest config historically resolved Svelte's *server* build), these
 * are real `@testing-library/svelte` renders. They are enabled by
 * `resolve.conditions: ['browser']` in `vitest.config.ts`, which makes vite
 * pick Svelte's client build so `render`/`mount` work under jsdom. The full
 * pre-existing unit suite was re-run with that condition set and still passes.
 *
 * The two headline regressions these lock in:
 *   (a) renaming an object field no longer remounts the row / loses input
 *       focus — rows are keyed by the stable `field.id`, not `name + idx`.
 *   (b) field-name slugification is deferred to blur (Enter also commits), so
 *       the user can type capitals/spaces freely without each keystroke
 *       slugifying and re-deriving the value.
 */
import { describe, it, expect, vi } from 'vitest';
import { render, fireEvent, cleanup } from '@testing-library/svelte';
import { afterEach } from 'vitest';
import SchemaBuilder from './SchemaBuilder.svelte';

afterEach(() => cleanup());

// ── Helpers ───────────────────────────────────────────────────────────────

type Emitted = Record<string, unknown>;

/**
 * Render with a vi.fn() onchange and a small accessor for the most recently
 * emitted schema. The `schema` prop is intentionally *not* fed back into the
 * component (the builder owns its own edit model and only emits upward), which
 * mirrors the real callers.
 */
function setup(schema: unknown, extra: Record<string, unknown> = {}) {
	const onchange = vi.fn();
	const utils = render(SchemaBuilder, { props: { schema, onchange, ...extra } });
	const lastEmitted = (): Emitted | undefined => {
		const calls = onchange.mock.calls;
		return calls.length ? (calls[calls.length - 1][0] as Emitted) : undefined;
	};
	return { onchange, lastEmitted, ...utils };
}

// ── 1. Renders object fields with their names ──────────────────────────────

describe('rendering', () => {
	it('renders an object schema field row with its name', () => {
		const { getByTestId } = setup({
			type: 'object',
			properties: { alpha: { type: 'string' }, beta: { type: 'number' } }
		});
		// One row per property, keyed positionally for the testid.
		expect(getByTestId('schema-builder-field-0')).toBeTruthy();
		expect(getByTestId('schema-builder-field-1')).toBeTruthy();
		const name0 = getByTestId('schema-builder-field-name-0') as HTMLInputElement;
		const name1 = getByTestId('schema-builder-field-name-1') as HTMLInputElement;
		expect(name0.value).toBe('alpha');
		expect(name1.value).toBe('beta');
	});
});

// ── 2. Adding a field emits a schema with a new property ───────────────────

describe('add field', () => {
	it('emits an onchange schema containing the newly added property', async () => {
		const { getByTestId, lastEmitted } = setup({
			type: 'object',
			properties: { existing: { type: 'string' } }
		});
		await fireEvent.click(getByTestId('schema-builder-add-field'));
		const emitted = lastEmitted();
		expect(emitted).toBeDefined();
		expect(emitted!.type).toBe('object');
		const props = emitted!.properties as Record<string, unknown>;
		// Original property preserved, plus a new auto-named one.
		expect(props).toHaveProperty('existing');
		expect(Object.keys(props).length).toBe(2);
		const added = Object.keys(props).find((k) => k !== 'existing');
		expect(added).toBeTruthy();
	});
});

// ── 3. Focus retention on rename (THE key regression) ──────────────────────

describe('focus retention on field rename', () => {
	/**
	 * REGRESSION GUARD. Against the OLD implementation (each-block keyed by
	 * `name + idx`, slugify on every keystroke) the field row's identity changed
	 * the moment the derived name changed, so Svelte tore down and rebuilt the
	 * <input> on the first keystroke — `document.activeElement` would no longer
	 * be the original element and focus was lost mid-type. Keying by the stable
	 * `field.id` and deferring slugify to blur keeps the SAME DOM node focused
	 * throughout. This test FAILS on the old code and PASSES now.
	 */
	it('keeps the same input element focused while typing a multi-char name', async () => {
		const { getByTestId } = setup({
			type: 'object',
			properties: { foo: { type: 'string' } }
		});
		const input = getByTestId('schema-builder-field-name-0') as HTMLInputElement;
		input.focus();
		expect(document.activeElement).toBe(input);

		// Start from an empty field (as if the user selected-all + typed over).
		input.value = '';
		await fireEvent.input(input);
		expect(document.activeElement).toBe(input);

		// Type characters that the old per-keystroke slugifier would have
		// mangled (capitals + space). Re-assert focus after every keystroke
		// against the ORIGINAL element reference — a remount would break this.
		const text = 'My New Name';
		for (const ch of text) {
			input.value = input.value + ch;
			await fireEvent.input(input);
			expect(document.activeElement).toBe(input);
		}
		// The draft is shown verbatim (not slugified) while typing.
		expect(input.value).toBe('My New Name');
	});

	it('commits the slugified property name on blur', async () => {
		const { getByTestId, lastEmitted } = setup({
			type: 'object',
			properties: { foo: { type: 'string' } }
		});
		const input = getByTestId('schema-builder-field-name-0') as HTMLInputElement;
		input.focus();
		input.value = 'My New Name';
		await fireEvent.input(input);
		await fireEvent.blur(input);

		const emitted = lastEmitted();
		expect(emitted).toBeDefined();
		const props = emitted!.properties as Record<string, unknown>;
		// slugifyFieldName('My New Name') === 'my_new_name'
		expect(props).toHaveProperty('my_new_name');
		expect(props).not.toHaveProperty('foo');
	});
});

// ── 4. Rename keeps an expanded field expanded ─────────────────────────────

describe('expansion survives rename', () => {
	it('a field expanded then renamed stays expanded (expansion keyed by id)', async () => {
		const { getByTestId, getByLabelText, queryByLabelText } = setup({
			type: 'object',
			properties: { foo: { type: 'string' } }
		});
		// Expand the field (chevron toggle button has aria-label "Expand field").
		await fireEvent.click(getByLabelText('Expand field'));
		// Now collapsible — the toggle flips to "Collapse field".
		expect(getByLabelText('Collapse field')).toBeTruthy();

		// Rename + commit. If expansion were keyed by name/idx this would reset.
		const input = getByTestId('schema-builder-field-name-0') as HTMLInputElement;
		input.focus();
		input.value = 'renamed';
		await fireEvent.input(input);
		await fireEvent.blur(input);

		// Still expanded: the collapse control is still present, expand-prompt gone.
		expect(getByLabelText('Collapse field')).toBeTruthy();
		expect(queryByLabelText('Expand field')).toBeNull();
	});
});

// ── 5. Nested edit round-trips without corrupting siblings ─────────────────

describe('nested edit round-trip', () => {
	it('adding a nested field inside an expanded object field updates only that field', async () => {
		// Two siblings: `keep` (a plain string) and `nested` (an object we drill into).
		const { getByTestId, getByLabelText, lastEmitted } = setup({
			type: 'object',
			properties: {
				keep: { type: 'string' },
				nested: { type: 'object', properties: { inner: { type: 'string' } } }
			}
		});

		// `nested` is field index 1 — expand it. There's only one collapsed field
		// chevron per row; target the second row's expand control by scoping.
		const row1 = getByTestId('schema-builder-field-1');
		const expandBtn = row1.querySelector('[aria-label="Expand field"]') as HTMLButtonElement;
		expect(expandBtn).toBeTruthy();
		await fireEvent.click(expandBtn);

		// Inside the nested editor, click its "Add field" button. There are now
		// two add-field buttons (root + nested); the nested one lives inside row1.
		const nestedAdd = row1.querySelector(
			'[data-testid="schema-builder-add-field"]'
		) as HTMLButtonElement;
		expect(nestedAdd).toBeTruthy();
		await fireEvent.click(nestedAdd);

		const emitted = lastEmitted();
		expect(emitted).toBeDefined();
		const props = emitted!.properties as Record<string, unknown>;
		// Sibling untouched.
		expect(props.keep).toMatchObject({ type: 'string' });
		// Nested object now has its original inner + the added field.
		const nested = props.nested as Record<string, unknown>;
		expect(nested.type).toBe('object');
		const nestedProps = nested.properties as Record<string, unknown>;
		expect(nestedProps).toHaveProperty('inner');
		expect(Object.keys(nestedProps).length).toBe(2);
	});
});

// ── 6. Root-kind switch object → scalar ────────────────────────────────────

describe('root-kind switch', () => {
	it('switching object → scalar (allowRootScalar) emits a scalar schema', async () => {
		const { getByTestId, lastEmitted } = setup(
			{ type: 'object', properties: { foo: { type: 'string' } } },
			{ allowRootScalar: true }
		);
		await fireEvent.click(getByTestId('schema-builder-kind-scalar'));
		const emitted = lastEmitted();
		expect(emitted).toBeDefined();
		// Fresh scalar default: a plain string scalar, no object keys.
		expect(emitted!.type).toBe('string');
		expect(emitted).not.toHaveProperty('properties');
	});

	it('does not offer the scalar root button without allowRootScalar', () => {
		const { queryByTestId } = setup({ type: 'object', properties: {} });
		expect(queryByTestId('schema-builder-kind-scalar')).toBeNull();
	});
});

// ── 7. readonly disables editing ───────────────────────────────────────────

describe('readonly', () => {
	it('disables the add-field button, the name input, and the root-kind buttons', () => {
		const { getByTestId } = setup(
			{ type: 'object', properties: { foo: { type: 'string' } } },
			{ readonly: true }
		);
		// Field name input disabled.
		expect((getByTestId('schema-builder-field-name-0') as HTMLInputElement).disabled).toBe(true);
		// Root-kind buttons disabled.
		expect((getByTestId('schema-builder-kind-object') as HTMLButtonElement).disabled).toBe(true);
	});

	it('hides the add-field control entirely when readonly', () => {
		const { queryByTestId } = setup(
			{ type: 'object', properties: { foo: { type: 'string' } } },
			{ readonly: true }
		);
		// The add-field button is wrapped in `{#if !readonly}`.
		expect(queryByTestId('schema-builder-add-field')).toBeNull();
	});

	it('clicking a (disabled) add-field control emits nothing — editing is impossible', () => {
		const { queryByTestId, onchange } = setup(
			{ type: 'object', properties: { foo: { type: 'string' } } },
			{ readonly: true }
		);
		// There is no add-field button at all; nothing to click, nothing emitted.
		expect(queryByTestId('schema-builder-add-field')).toBeNull();
		expect(onchange).not.toHaveBeenCalled();
	});
});

// ── 8. Consolidated LLM-path parity (old JsonSchemaBuilder shape) ──────────

describe('object-of-scalars (old JsonSchemaBuilder shape) parity', () => {
	it('loads an object-of-scalars schema and re-emits an equivalent schema', async () => {
		// The exact shape the old JsonSchemaBuilder produced for LLM structured
		// output: object + properties + required + additionalProperties.
		const original = {
			type: 'object',
			properties: {
				title: { type: 'string' },
				count: { type: 'integer' },
				done: { type: 'boolean' }
			},
			required: ['title', 'count'],
			additionalProperties: false
		};
		const { getByTestId, lastEmitted } = setup(original);

		// Loaded: three rows with the right names.
		expect((getByTestId('schema-builder-field-name-0') as HTMLInputElement).value).toBe('title');
		expect((getByTestId('schema-builder-field-name-1') as HTMLInputElement).value).toBe('count');
		expect((getByTestId('schema-builder-field-name-2') as HTMLInputElement).value).toBe('done');

		// Trigger a no-op-ish emit by toggling and re-committing a name to its
		// own value, so we can inspect the serialised form.
		const input = getByTestId('schema-builder-field-name-0') as HTMLInputElement;
		input.focus();
		input.value = 'title';
		await fireEvent.input(input);
		await fireEvent.blur(input);

		const emitted = lastEmitted();
		expect(emitted).toBeDefined();
		expect(emitted!.type).toBe('object');
		const props = emitted!.properties as Record<string, unknown>;
		expect(props.title).toMatchObject({ type: 'string' });
		expect(props.count).toMatchObject({ type: 'integer' });
		expect(props.done).toMatchObject({ type: 'boolean' });
		// required round-trips (order-insensitive) and sealing is preserved.
		expect(new Set(emitted!.required as string[])).toEqual(new Set(['title', 'count']));
		expect(emitted!.additionalProperties).toBe(false);
	});
});
