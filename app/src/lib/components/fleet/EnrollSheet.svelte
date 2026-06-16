<script lang="ts">
	// Reusable "Enroll a runner" sheet — the token-mint + reveal-once (CLI command)
	// stages extracted from RunnerList. Two callers:
	//
	//   global    — opened from the Control Plane's Presence section with NO fixed
	//               group; shows a group picker over the presence `capacity`
	//               resources (the backed runner groups).
	//   per-card  — opened from a CapacityCard's "Enroll here" with `group` fixed to
	//               that capacity's path; no picker, the minted token is scoped to it.
	//
	// Group CREATION is no longer here — that's NewCapacityModal's job. A runner can
	// only be enrolled into an EXISTING backed group (or no group at all, in the
	// global flow). On submit it mints the `rt_` token and discloses it ONCE with a
	// ready-to-paste `aithericon-executor register` command.
	import {
		Sheet,
		SheetContent,
		SheetTitle,
		SheetDescription,
		SheetClose
	} from '$lib/components/ui/sheet';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { CopyButton } from '$lib/components/ui/copy-button';
	import * as Select from '$lib/components/ui/select';
	import { toast } from 'svelte-sonner';
	import Plus from '@lucide/svelte/icons/plus';
	import Terminal from '@lucide/svelte/icons/terminal';
	import TriangleAlert from '@lucide/svelte/icons/triangle-alert';
	import Globe from '@lucide/svelte/icons/globe';
	import {
		createRegistrationToken,
		type CreatedRegistrationToken
	} from '$lib/api/runners';
	import { createWorkerRegistrationToken } from '$lib/api/workers';
	import { listCapacities, type CapacitySummary } from '$lib/api/capacities';

	type Props = {
		open: boolean;
		/** When set, the token is scoped to THIS group (no picker is shown). When
		 *  omitted, the sheet shows a picker over the presence `capacity` groups. */
		group?: string | null;
		/** Whether the fixed `group` is a platform-tier (shared) pool. Set by the
		 *  per-pool caller from the pool's `scope_kind`. When true the minted token
		 *  is platform-scoped (`platform: true`). Enrolling INTO a platform pool is
		 *  platform-admin curation, so the caller MUST only expose this affordance
		 *  to platform admins (the backend also 403s a non-admin platform mint).
		 *  The global picker never offers platform pools, so this only applies to
		 *  the fixed-group flow. */
		groupIsPlatform?: boolean;
		/** Which fleet this sheet enrolls into. `'runner'` (default) mints an `rt_`
		 *  runner token over the presence `runner_group` capacities; `'worker'`
		 *  mints a `wt_` worker token over the competing-consumer `workers`
		 *  capacities. The form/reveal flow is otherwise identical. */
		mode?: 'runner' | 'worker';
		/** Called after a successful mint (parent may refresh its token list). */
		onenrolled?: () => void;
	};

	let {
		open = $bindable(),
		group = null,
		groupIsPlatform = false,
		mode = 'runner',
		onenrolled
	}: Props = $props();

	const fixedGroup = $derived(group ?? null);
	const isWorker = $derived(mode === 'worker');
	const unit = $derived(isWorker ? 'worker' : 'runner');
	// Platform-ness is DEFINED BY THE POOL, not chosen here: only a fixed-group
	// enrol into a platform pool mints a platform token. The global picker filters
	// platform pools out entirely (you enrol into a shared pool from its own page,
	// which is admin-gated), so the picker flow is never platform.
	const enrolIntoPlatform = $derived(!!fixedGroup && groupIsPlatform);

	// ── Form state ───────────────────────────────────────────────────────────────
	let name = $state('');
	let groupSel = $state('');
	let maxUses = $state('');
	let reusable = $state(false);
	let expiresAt = $state('');
	let enrolling = $state(false);

	// Group picker source (global flow only) — the workspace `capacity` groups of
	// the matching backend. Platform pools are EXCLUDED (`scope_kind === 'platform'`):
	// enrolling compute into a shared pool is platform-admin curation, done from the
	// pool's own (admin-gated) page, not from this generic picker.
	let groups = $state<CapacitySummary[]>([]);

	// Reveal-once token.
	let revealed = $state<(CreatedRegistrationToken & { name: string; group: string }) | null>(null);

	// Reset + (when global) load the group list each time the sheet opens.
	$effect(() => {
		if (!open) return;
		name = '';
		groupSel = '';
		maxUses = '';
		reusable = false;
		expiresAt = '';
		if (!fixedGroup) {
			(async () => {
				try {
					const all = await listCapacities();
					const wantBackend = isWorker ? 'queue' : 'presence';
					groups = all.filter((c) => c.backend === wantBackend && c.scope_kind !== 'platform');
				} catch {
					groups = [];
				}
			})();
		}
	});

	// ── Mint ──────────────────────────────────────────────────────────────────────
	async function handleEnroll(e: Event) {
		e.preventDefault();
		if (enrolling) return;
		enrolling = true;
		try {
			// A fixed group wins; otherwise the picker value ('' ⇒ no group).
			const resolvedGroup = fixedGroup ?? (groupSel || undefined);
			const mint = isWorker ? createWorkerRegistrationToken : createRegistrationToken;
			const created = await mint({
				group: resolvedGroup,
				// Always send the explicit checkbox value: the backend defaults an
				// OMITTED `reusable` to `true`.
				reusable,
				max_uses: maxUses ? parseInt(maxUses, 10) : undefined,
				expires_at: expiresAt ? `${expiresAt}T23:59:59Z` : undefined,
				// Platform-ness comes from the target pool, not a per-enrol choice:
				// only a fixed-group enrol into a platform pool mints a platform
				// token. The backend 403s a non-admin platform mint as defence in depth.
				platform: enrolIntoPlatform ? true : undefined
			});
			revealed = { ...created, name: name.trim(), group: resolvedGroup ?? '' };
			open = false;
			toast.success('Token minted — copy it now.');
			onenrolled?.();
		} catch (err) {
			toast.error(`Enroll failed: ${err instanceof Error ? err.message : err}`);
		} finally {
			enrolling = false;
		}
	}

	/** Build the CLI enroll line shown in the reveal sheet. */
	function cliLine(token: string, unitName: string, unitGroup: string): string {
		const origin = typeof window !== 'undefined' ? window.location.origin : '';
		if (isWorker) {
			// Workers self-enroll on boot: the executor daemon reads its `wt_`
			// registration token from the environment and POSTs /api/v1/workers/enroll
			// (the group is inherited from the token — no `--group` flag).
			let cmd = `EXECUTOR_MEKHAN_URL=${origin} EXECUTOR_WORKER_REG_TOKEN=${token}`;
			if (unitName) cmd += ` EXECUTOR_WORKER_NAME=${unitName}`;
			cmd += ' aithericon-executor';
			return cmd;
		}
		let cmd = `aithericon-executor register --url ${origin} --token ${token}`;
		if (unitName) cmd += ` --name ${unitName}`;
		if (unitGroup) cmd += ` --group ${unitGroup}`;
		return cmd;
	}
</script>

<!-- ── Enroll modal (mint registration token) ──────────────────────────────── -->
<Sheet.Root
	{open}
	onOpenChange={(o: boolean) => {
		if (!o) open = false;
	}}
>
	<SheetContent class="w-[480px] sm:max-w-[480px]">
		<div class="space-y-4 p-2" data-testid="enroll-sheet">
			<div class="space-y-1">
				<SheetTitle class="flex items-center gap-2 text-lg font-semibold">
					<Plus class="size-4" />
					{fixedGroup ? `Enroll a ${unit} into ${fixedGroup}` : `Enroll a new ${unit}`}
				</SheetTitle>
				<SheetDescription class="text-sm text-muted-foreground">
					Mint a one-time registration token. Hand it to the executor — it enrolls itself using
					<code class="rounded bg-muted px-1 py-0.5 font-mono text-sm">
						aithericon-executor register
					</code>.
				</SheetDescription>
			</div>

			<form class="space-y-3" onsubmit={handleEnroll}>
				<div class="space-y-1">
					<label
						for="enroll-name"
						class="text-sm font-medium uppercase tracking-wide text-muted-foreground"
					>
						{isWorker ? 'Worker name' : 'Runner name'}
					</label>
					<Input id="enroll-name" bind:value={name} required placeholder="e.g. gpu-node-01" />
					<p class="text-sm text-muted-foreground">
						Required — the generated <code>register</code> command needs <code>--name</code>.
					</p>
				</div>

				{#if fixedGroup}
					<div class="space-y-1">
						<span class="text-sm font-medium uppercase tracking-wide text-muted-foreground">
							Group
						</span>
						<p class="rounded-md border border-border/60 bg-muted/40 px-2 py-1.5 font-mono text-sm">
							{fixedGroup}
						</p>
						<p class="text-sm text-muted-foreground">
							{isWorker
								? "The worker competes for this group's queued jobs."
								: "The runner's unit is admitted into this presence group's pool net."}
						</p>
					</div>
				{:else}
					<div class="space-y-1">
						<label
							for="enroll-group"
							class="text-sm font-medium uppercase tracking-wide text-muted-foreground"
						>
							Group
						</label>
						<Select.Root
							type="single"
							value={groupSel}
							onValueChange={(v) => (groupSel = v ?? '')}
						>
							<Select.Trigger id="enroll-group" class="h-9 w-full" data-testid="enroll-group-select">
								{groupSel || 'No group'}
							</Select.Trigger>
							<Select.Content>
								<Select.Item value="" label="No group" />
								{#each groups as g (g.id)}
									<Select.Item value={g.path} label={g.path} />
								{/each}
							</Select.Content>
						</Select.Root>
						<p class="text-sm text-muted-foreground">
							{#if isWorker}
								Pick a <code class="font-mono">workers</code> group — create new groups from
								<strong>New capacity</strong>.
							{:else}
								Pick a backed presence <code class="font-mono">capacity</code> group — create new
								groups from <strong>New capacity</strong>.
							{/if}
						</p>
					</div>
				{/if}

				<div class="grid gap-3 sm:grid-cols-2">
					<div class="space-y-1">
						<label
							for="enroll-max"
							class="text-sm font-medium uppercase tracking-wide text-muted-foreground"
						>
							Max uses <span class="normal-case">(optional)</span>
						</label>
						<Input
							id="enroll-max"
							type="number"
							min="1"
							bind:value={maxUses}
							placeholder="unlimited"
						/>
					</div>
					<div class="space-y-1">
						<label
							for="enroll-expires"
							class="text-sm font-medium uppercase tracking-wide text-muted-foreground"
						>
							Expires <span class="normal-case">(optional)</span>
						</label>
						<Input id="enroll-expires" type="date" bind:value={expiresAt} />
					</div>
				</div>

				<div class="flex items-center gap-2">
					<input
						id="enroll-reusable"
						type="checkbox"
						bind:checked={reusable}
						class="size-4 rounded border-border"
					/>
					<label for="enroll-reusable" class="text-sm text-muted-foreground"> Reusable token </label>
				</div>

				{#if enrolIntoPlatform}
					<div
						class="flex items-start gap-2.5 rounded-md border border-sky-200 bg-sky-50/60 p-3 text-sm"
						data-testid="enroll-platform-note"
					>
						<Globe class="mt-0.5 size-3.5 shrink-0 text-sky-600" />
						<span>
							<span class="font-medium text-foreground">Shared platform pool</span>
							<span class="text-muted-foreground">
								— this token enrols the {unit} into the global platform pool, runnable by every
								workspace.
							</span>
						</span>
					</div>
				{/if}

				<div class="flex gap-2 pt-1">
					<Button type="submit" disabled={enrolling || !name.trim()} class="flex-1">
						{enrolling ? 'Minting…' : 'Mint token'}
					</Button>
					<SheetClose>
						<Button type="button" variant="outline">Cancel</Button>
					</SheetClose>
				</div>
			</form>
		</div>
	</SheetContent>
</Sheet.Root>

<!-- ── Reveal-once token sheet ─────────────────────────────────────────────── -->
<Sheet.Root
	open={revealed !== null}
	onOpenChange={(o: boolean) => {
		if (!o) revealed = null;
	}}
>
	<SheetContent class="w-[520px] sm:max-w-[520px]">
		<div class="space-y-4 p-2">
			<div class="space-y-1">
				<SheetTitle class="flex items-center gap-2 text-lg font-semibold">
					<Terminal class="size-4" />
					Registration token
				</SheetTitle>
				<SheetDescription class="text-sm text-muted-foreground">
					Copy this now — it is not stored and will never be shown again.
				</SheetDescription>
			</div>

			<div
				class="flex items-start gap-2 rounded-md border border-amber-500/40 bg-amber-500/10 p-2 text-sm text-amber-700 dark:text-amber-400"
			>
				<TriangleAlert class="mt-0.5 size-3.5 shrink-0" />
				<span>Anyone with this token can enroll an executor that acts on your behalf.</span>
			</div>

			{#if revealed}
				<!-- Token secret -->
				<div>
					<p class="mb-1 text-sm font-medium uppercase tracking-wide text-muted-foreground">Token</p>
					<div class="flex items-center gap-2">
						<code
							class="flex-1 break-all rounded bg-muted px-2 py-1.5 font-mono text-sm text-foreground"
							data-testid="token-secret"
						>
							{revealed.token}
						</code>
						<CopyButton text={revealed.token} />
					</div>
				</div>

				<!-- CLI enroll line -->
				<div>
					<p class="mb-1 text-sm font-medium uppercase tracking-wide text-muted-foreground">
						Ready-to-paste CLI command
					</p>
					<div class="flex items-start gap-2">
						<code class="flex-1 break-all rounded bg-muted px-2 py-1.5 font-mono text-sm text-foreground">
							{cliLine(revealed.token, revealed.name, revealed.group)}
						</code>
						<CopyButton text={cliLine(revealed.token, revealed.name, revealed.group)} />
					</div>
				</div>
			{/if}

			<SheetClose>
				<Button variant="outline" class="w-full">Done</Button>
			</SheetClose>
		</div>
	</SheetContent>
</Sheet.Root>
