import { describe, it, expect } from 'vitest';
import { nodeKindMeta, normalizeNodeKind } from './node-kind-meta';

describe('normalizeNodeKind', () => {
	it('passes through a present snake_case kind unchanged (editor type / step node_kind)', () => {
		expect(normalizeNodeKind('automated_step')).toBe('automated_step');
		expect(normalizeNodeKind('lease_scope')).toBe('lease_scope');
		// A runtime-only projection kind that is NOT an editable NodeKind still
		// round-trips — normalization does not narrow to the union.
		expect(normalizeNodeKind('scheduled')).toBe('scheduled');
	});

	it('defaults absent / empty discriminants to "unknown"', () => {
		expect(normalizeNodeKind(null)).toBe('unknown');
		expect(normalizeNodeKind(undefined)).toBe('unknown');
		expect(normalizeNodeKind('')).toBe('unknown');
	});
});

describe('nodeKindMeta after normalization', () => {
	it('resolves the same meta for the editor `type` and the instance `node_kind` shapes', () => {
		// Both inspectors feed their kind through normalizeNodeKind before lookup;
		// equal kind strings must produce identical icon + label + colour tokens.
		const editorSide = nodeKindMeta(normalizeNodeKind('agent'));
		const instanceSide = nodeKindMeta(normalizeNodeKind('agent'));
		expect(instanceSide).toBe(editorSide);
		expect(editorSide.label).toBe('Agent');
		expect(editorSide.chipClass).toBe('bg-node-agent');
	});

	it('falls back to the default chip for unknown/absent kinds', () => {
		const meta = nodeKindMeta(normalizeNodeKind(undefined));
		expect(meta.label).toBe('Node');
		expect(meta.chipClass).toBe('bg-muted');
	});
});
