<script lang="ts" module>
	import type { RunnerPresenceSnapshot } from '$lib/api/models';

	export type { RunnerPresenceSnapshot };

	/**
	 * Does this presence row advertise the given executor backend wire-name?
	 * Exposed so callers can gate an action against the SELECTED runner — e.g. the
	 * catalog disables an `hf.co` provision when the target lacks `ollama`.
	 */
	export function runnerAdvertises(
		runner: RunnerPresenceSnapshot | undefined,
		backend: string
	): boolean {
		return !!runner && (runner.backends ?? []).includes(backend);
	}
</script>

<script lang="ts">
	// Reusable picker over the LIVE runner presence snapshot. Polls
	// `GET /api/v1/runners/presence` on mount + every 5s, filters to the runners
	// mekhan currently considers PRESENT, and lets the operator pick a target
	// runner to act against (load/unload a model, provision from a catalog, …).
	//
	// Fail-soft: a fetch error leaves the current list in place (no throw, no
	// flicker) so a transient blip doesn't wipe the operator's selection.

	import * as Select from '$lib/components/ui/select';
	import { Badge } from '$lib/components/ui/badge';
	import { listRunnerPresence } from '$lib/api/models';

	type Props = {
		/** The selected runner id, or `null` when nothing is chosen yet. */
		value: string | null;
		/** Called with the chosen runner id. */
		onChange: (runnerId: string) => void;
		/**
		 * When set, surfaced to callers via {@link runnerAdvertises}; the picker
		 * itself does NOT hide runners lacking it (so the operator can still see why
		 * an action is disabled), but a caller can read `selectedAdvertises` below.
		 */
		requireBackend?: string;
		/** Optional reason the whole picker is disabled (e.g. no runners present). */
		disabledReason?: string;
	};

	let { value, onChange, requireBackend, disabledReason }: Props = $props();

	let runners = $state<RunnerPresenceSnapshot[]>([]);

	async function poll() {
		try {
			const all = await listRunnerPresence();
			runners = all.filter((r) => r.present === true);
		} catch {
			// fail-soft — keep whatever we had
		}
	}

	$effect(() => {
		poll();
		const id = setInterval(poll, 5000);
		return () => clearInterval(id);
	});

	// Auto-select the first present runner when the bound value is empty or has
	// gone stale (the previously-selected runner dropped out of presence).
	$effect(() => {
		const present = runners;
		if (present.length === 0) return;
		const stillValid = value != null && present.some((r) => r.runner_id === value);
		if (!stillValid) onChange(present[0].runner_id);
	});

	const selected = $derived(runners.find((r) => r.runner_id === value));
	/**
	 * Whether the SELECTED runner advertises `requireBackend` (true if none
	 * required). Exported as a function so callers can `bind:this` the picker and
	 * gate an action against the live selection.
	 */
	export function selectedAdvertises(): boolean {
		return requireBackend === undefined ? true : runnerAdvertises(selected, requireBackend);
	}

	function shortId(id: string): string {
		return id.length > 8 ? id.slice(0, 8) : id;
	}

	function triggerLabel(): string {
		if (runners.length === 0) return disabledReason ?? 'No runners present';
		if (!selected) return 'Select a runner…';
		return shortId(selected.runner_id);
	}
</script>

<Select.Root
	type="single"
	value={value ?? ''}
	onValueChange={(v) => v && onChange(v)}
	disabled={runners.length === 0 || disabledReason !== undefined}
>
	<Select.Trigger
		disabled={runners.length === 0 || disabledReason !== undefined}
		data-testid="runner-target-picker"
	>
		<span class="truncate font-mono text-sm">{triggerLabel()}</span>
	</Select.Trigger>
	<Select.Content>
		{#each runners as r (r.runner_id)}
			<Select.Item value={r.runner_id} label={shortId(r.runner_id)}>
				<span class="font-mono text-sm">{shortId(r.runner_id)}</span>
				{#each r.backends ?? [] as b (b)}
					<Badge variant="secondary" class="font-mono text-xs">{b}</Badge>
				{/each}
			</Select.Item>
		{/each}
	</Select.Content>
</Select.Root>
