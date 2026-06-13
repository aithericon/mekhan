// Single source of truth for every "status badge" on the surface.
//
// Before this module the same status→color logic was hand-rolled in ~12 places
// (instance list, home, nets, editor runs menu, steps table, processes, data
// copies, tasks, cluster leases, template staging, …) with drifting palettes
// and patchy dark-mode support. Consolidating here means:
//
//   1. ONE dark-mode-aware palette, keyed by a small set of semantic *tones*.
//   2. Per-domain maps that say "this domain's `running` is the `info` tone",
//      so two domains that spell a state differently ("running" vs "active")
//      still render identically, and one domain can map a shared word to a
//      different tone ("pending" is neutral for a step but a warning for a task)
//      without copy-pasting tailwind classes.
//
// `StatusBadge.svelte` is the only renderer; everything else feeds it a
// (domain, status) pair. Tailwind classes are written as full literal strings
// so the JIT keeps them.

export type StatusTone =
	| 'neutral'
	| 'info'
	| 'success'
	| 'danger'
	| 'muted'
	| 'warn'
	| 'caution'
	| 'accent';

export interface ToneStyle {
	/** Soft pill: background + text, with a dark-mode variant. */
	pill: string;
	/** Solid leading dot colour. */
	dot: string;
}

// The shared palette. Modeled on the instance-header pills the design settled
// on: a soft 100/700 light pill, a translucent 500/15 dark pill, and a solid
// 500 dot. Every domain resolves into exactly these tones.
export const TONES: Record<StatusTone, ToneStyle> = {
	neutral: {
		pill: 'bg-gray-100 text-gray-700 dark:bg-gray-500/15 dark:text-gray-300',
		dot: 'bg-gray-400'
	},
	info: {
		pill: 'bg-blue-100 text-blue-700 dark:bg-blue-500/15 dark:text-blue-300',
		dot: 'bg-blue-500'
	},
	success: {
		pill: 'bg-green-100 text-green-700 dark:bg-green-500/15 dark:text-green-300',
		dot: 'bg-green-500'
	},
	danger: {
		pill: 'bg-red-100 text-red-700 dark:bg-red-500/15 dark:text-red-300',
		dot: 'bg-red-500'
	},
	muted: {
		pill: 'bg-slate-100 text-slate-700 dark:bg-slate-500/15 dark:text-slate-300',
		dot: 'bg-slate-400'
	},
	warn: {
		pill: 'bg-amber-100 text-amber-700 dark:bg-amber-500/15 dark:text-amber-300',
		dot: 'bg-amber-500'
	},
	caution: {
		pill: 'bg-orange-100 text-orange-700 dark:bg-orange-500/15 dark:text-orange-300',
		dot: 'bg-orange-500'
	},
	accent: {
		pill: 'bg-cyan-100 text-cyan-700 dark:bg-cyan-500/15 dark:text-cyan-300',
		dot: 'bg-cyan-500'
	}
};

/** A status' presentation within a domain. */
export interface StatusSpec {
	tone: StatusTone;
	/** Display label; defaults to the raw status key when omitted. */
	label?: string;
	/** Pulse the leading dot — for in-flight states (running, staging). */
	pulse?: boolean;
}

/**
 * The domains that own a status vocabulary. Each maps its own status strings to
 * a tone. Add a domain here rather than re-deriving colors at a call site.
 */
export type StatusDomain =
	| 'workflow' // workflow instance / net run lifecycle
	| 'step' // per-step execution within a run
	| 'process' // HPI process lifecycle
	| 'task' // human task / inbox item
	| 'lease' // cluster capacity lease (allocation)
	| 'copy' // file-copy / catalogue replica state
	| 'staging'; // template staging to a remote scheduler

const REGISTRY: Record<StatusDomain, Record<string, StatusSpec>> = {
	workflow: {
		created: { tone: 'neutral' },
		running: { tone: 'info', pulse: true },
		completed: { tone: 'success' },
		failed: { tone: 'danger' },
		cancelled: { tone: 'muted' }
	},
	step: {
		pending: { tone: 'neutral' },
		running: { tone: 'info', pulse: true },
		completed: { tone: 'success' },
		failed: { tone: 'danger' },
		skipped: { tone: 'muted' }
	},
	process: {
		active: { tone: 'info', pulse: true },
		running: { tone: 'info', pulse: true },
		completed: { tone: 'success' },
		failed: { tone: 'danger' }
	},
	task: {
		pending: { tone: 'warn', label: 'Pending' },
		completed: { tone: 'success', label: 'Completed' },
		cancelled: { tone: 'muted', label: 'Cancelled' },
		failed: { tone: 'danger', label: 'Rejected' }
	},
	lease: {
		pending: { tone: 'warn' },
		held: { tone: 'success' },
		released: { tone: 'muted' },
		failed: { tone: 'danger' },
		expired: { tone: 'caution' }
	},
	copy: {
		indexed: { tone: 'muted' },
		verified: { tone: 'success' },
		registered: { tone: 'info' },
		copied: { tone: 'accent' },
		deleted: { tone: 'neutral' },
		mismatch: { tone: 'danger' },
		orphan_disk: { tone: 'warn' },
		orphan_db: { tone: 'caution' }
	},
	staging: {
		staging: { tone: 'warn', pulse: true },
		staged: { tone: 'success' },
		failed: { tone: 'danger' },
		stale: { tone: 'muted' }
	}
};

export interface ResolvedStatus {
	tone: StatusTone;
	style: ToneStyle;
	label: string;
	pulse: boolean;
}

/**
 * Resolve a (domain, status) pair into its tone, palette, label and pulse.
 * Unknown statuses fall back to the neutral tone with the raw key as label, so
 * a new backend state degrades gracefully instead of rendering unstyled.
 */
export function resolveStatus(domain: StatusDomain, status: string | null | undefined): ResolvedStatus {
	const key = (status ?? '').toLowerCase();
	const spec = REGISTRY[domain]?.[key];
	const tone = spec?.tone ?? 'neutral';
	return {
		tone,
		style: TONES[tone],
		label: spec?.label ?? (status ?? '—'),
		pulse: spec?.pulse ?? false
	};
}
