<script lang="ts">
	// Enroll / edit a HUMAN into a consent-acceptance capacity (docs/33 P4). The
	// human counterpart to EnrollSheet — but humans don't mint a registration
	// token: an admin enrolls an EXISTING `workspace_member` directly onto the
	// roster (`POST /api/v1/roster`), so the picker is over the workspace's
	// members rather than over capacity groups.
	//
	// Two modes, switched on the `member` prop:
	//   ENROLL  (member null/absent) — member picker shown; submit POSTs a new row.
	//   EDIT    (member set)         — member locked/hidden; submit PATCHes that row
	//                                  by `member.id`.
	import {
		Sheet,
		SheetContent,
		SheetTitle,
		SheetDescription,
		SheetClose
	} from '$lib/components/ui/sheet';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import * as Select from '$lib/components/ui/select';
	import { toast } from 'svelte-sonner';
	import UserPlus from '@lucide/svelte/icons/user-plus';
	import Pencil from '@lucide/svelte/icons/pencil';
	import { auth } from '$lib/auth/store.svelte';
	import { listWorkspaceMembers, type WorkspaceMember } from '$lib/api/client';
	import {
		enrollMember,
		updateRosterMember,
		type AvailabilityConfig,
		type RosterMemberSummary
	} from '$lib/api/roster';
	import CapsEditor from './CapsEditor.svelte';

	type Props = {
		open: boolean;
		/** The human capacity this sheet enrolls into / edits within. */
		capacityId: string;
		/** Display name of that capacity (header copy only). */
		capacityName: string;
		/** When set → EDIT mode (member locked, PATCH by `member.id`). When
		 *  null/absent → ENROLL mode (member picker shown). */
		member?: RosterMemberSummary | null;
		/** Member user ids already enrolled — excluded from the ENROLL picker. */
		existingMemberIds?: string[];
		/** Called after a successful enroll/update so the parent can refetch. */
		ondone?: () => void;
	};

	let {
		open = $bindable(),
		capacityId,
		capacityName,
		member = null,
		existingMemberIds = [],
		ondone
	}: Props = $props();

	const isEdit = $derived(member != null);
	const workspaceId = $derived(auth.session?.user.workspaceId ?? '');
	const callerSubject = $derived(auth.session?.user.subject ?? '');

	// ── Availability presets ──────────────────────────────────────────────────
	// Two friendly modes + an advanced disclosure over the raw three fields.
	const SESSION_PRESET: AvailabilityConfig = {
		liveness_source: 'session',
		ttl_secs: 45,
		grace_secs: 15
	};
	const STICKY_PRESET: AvailabilityConfig = {
		liveness_source: 'none',
		ttl_secs: 0,
		grace_secs: 0
	};

	// ── Form state ─────────────────────────────────────────────────────────────
	let memberSel = $state('');
	let concurrency = $state(1);
	let caps = $state<Record<string, unknown>>({});
	let availability = $state<AvailabilityConfig>({ ...SESSION_PRESET });
	let submitting = $state(false);

	// Member picker source (ENROLL mode only).
	let members = $state<WorkspaceMember[]>([]);

	/** Which friendly preset the current availability matches, '' ⇒ custom. */
	const presetMode = $derived(
		availability.liveness_source === 'session' &&
			availability.ttl_secs === 45 &&
			availability.grace_secs === 15
			? 'session'
			: availability.liveness_source === 'none' &&
				  availability.ttl_secs === 0 &&
				  availability.grace_secs === 0
				? 'sticky'
				: 'custom'
	);

	function applyPreset(mode: 'session' | 'sticky') {
		availability = mode === 'session' ? { ...SESSION_PRESET } : { ...STICKY_PRESET };
	}

	// Reset + (re)load the form each time the sheet opens.
	$effect(() => {
		if (!open) return;
		submitting = false;
		if (member) {
			// EDIT — prefill from the summary row. The summary carries concurrency;
			// caps/availability aren't on the compact row, so start from sane
			// defaults (the PATCH only writes the fields the admin touches).
			memberSel = member.member_user_id;
			concurrency = member.concurrency ?? 1;
			caps = {};
			availability = { ...SESSION_PRESET };
		} else {
			memberSel = '';
			concurrency = 1;
			caps = {};
			availability = { ...SESSION_PRESET };
			void loadMembers();
		}
	});

	async function loadMembers() {
		if (!workspaceId) {
			members = [];
			return;
		}
		try {
			members = await listWorkspaceMembers(workspaceId);
		} catch {
			members = [];
		}
	}

	/** Enrollable members: everyone NOT already on this capacity's roster. */
	const candidates = $derived(
		members.filter((m) => !existingMemberIds.includes(m.user_id))
	);

	function memberLabel(m: WorkspaceMember): string {
		const name = m.display_name?.trim();
		const email = m.email?.trim();
		if (name && email) return `${name} · ${email}`;
		if (name) return name;
		if (email) return email;
		return m.user_id.slice(0, 8);
	}

	function short(id: string): string {
		return id.slice(0, 8);
	}

	const memberTriggerLabel = $derived.by(() => {
		if (!memberSel) return 'Select a member…';
		const m = candidates.find((c) => c.user_id === memberSel);
		return m ? memberLabel(m) : short(memberSel);
	});

	const canSubmit = $derived(
		!submitting && concurrency >= 1 && (isEdit || memberSel !== '')
	);

	async function handleSubmit(e: Event) {
		e.preventDefault();
		if (!canSubmit) return;
		submitting = true;
		try {
			if (isEdit && member) {
				await updateRosterMember(member.id, {
					concurrency,
					caps,
					availability
				});
				toast.success('Member updated.');
			} else {
				await enrollMember({
					capacity_id: capacityId,
					member_user_id: memberSel,
					concurrency,
					caps,
					availability
				});
				toast.success('Member enrolled.');
			}
			open = false;
			ondone?.();
		} catch (err) {
			const msg = err instanceof Error ? err.message : String(err);
			toast.error(`${isEdit ? 'Update' : 'Enroll'} failed: ${msg}`);
		} finally {
			submitting = false;
		}
	}
</script>

<Sheet.Root
	{open}
	onOpenChange={(o: boolean) => {
		if (!o) open = false;
	}}
>
	<SheetContent class="w-[480px] overflow-y-auto sm:max-w-[480px]">
		<div class="space-y-4 p-2" data-testid="human-enroll-sheet">
			<div class="space-y-1">
				<SheetTitle class="flex items-center gap-2 text-lg font-semibold">
					{#if isEdit}
						<Pencil class="size-4" />
						Edit member in {capacityName}
					{:else}
						<UserPlus class="size-4" />
						Enroll a member into {capacityName}
					{/if}
				</SheetTitle>
				<SheetDescription class="text-sm text-muted-foreground">
					{#if isEdit}
						Adjust this member's concurrency, capabilities, and how their presence is tracked.
					{:else}
						Pick a workspace member to add to this human pool. They'll appear offline until they
						mark themselves available.
					{/if}
				</SheetDescription>
			</div>

			<form class="space-y-4" onsubmit={handleSubmit}>
				<!-- Member -->
				<div class="space-y-1">
					<span class="text-sm font-medium uppercase tracking-wide text-muted-foreground">
						Member
					</span>
					{#if isEdit && member}
						<p
							class="rounded-md border border-border/60 bg-muted/40 px-2 py-1.5 text-sm"
							data-testid="enroll-member-locked"
						>
							{member.member_display_name ?? short(member.member_user_id)}
							{#if member.member_email}
								<span class="text-muted-foreground"> · {member.member_email}</span>
							{/if}
						</p>
					{:else}
						<Select.Root
							type="single"
							value={memberSel}
							onValueChange={(v) => (memberSel = v ?? '')}
						>
							<Select.Trigger
								class="h-9 w-full"
								data-testid="enroll-member-select"
							>
								{memberTriggerLabel}
							</Select.Trigger>
							<Select.Content>
								{#if candidates.length === 0}
									<div class="px-2 py-1.5 text-sm text-muted-foreground">
										No more members to enroll
									</div>
								{:else}
									{#each candidates as m (m.user_id)}
										<Select.Item value={m.user_id} label={memberLabel(m)}>
											<span class="flex items-center gap-1.5">
												{memberLabel(m)}
												{#if m.user_id === callerSubject}
													<span
														class="rounded-full bg-primary/10 px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wide text-primary"
													>
														you
													</span>
												{/if}
											</span>
										</Select.Item>
									{/each}
								{/if}
							</Select.Content>
						</Select.Root>
						<p class="text-sm text-muted-foreground">
							Only members already in this workspace can be enrolled.
						</p>
					{/if}
				</div>

				<!-- Concurrency -->
				<div class="space-y-1">
					<label
						for="enroll-concurrency"
						class="text-sm font-medium uppercase tracking-wide text-muted-foreground"
					>
						Concurrency
					</label>
					<Input
						id="enroll-concurrency"
						type="number"
						min="1"
						bind:value={concurrency}
						data-testid="enroll-concurrency"
					/>
					<p class="text-sm text-muted-foreground">
						How many offers this person can hold at once.
					</p>
				</div>

				<!-- Availability -->
				<div class="space-y-2">
					<span class="text-sm font-medium uppercase tracking-wide text-muted-foreground">
						Availability
					</span>
					<div class="space-y-1.5" role="radiogroup">
						<label class="flex items-start gap-2 text-sm">
							<input
								type="radio"
								name="availability-mode"
								class="mt-0.5 size-4"
								checked={presetMode === 'session'}
								onchange={() => applyPreset('session')}
							/>
							<span>
								<span class="font-medium text-foreground">Online while in a session</span>
								<span class="block text-xs text-muted-foreground">
									Presence follows an active mekhan session and lapses shortly after they leave.
								</span>
							</span>
						</label>
						<label class="flex items-start gap-2 text-sm">
							<input
								type="radio"
								name="availability-mode"
								class="mt-0.5 size-4"
								checked={presetMode === 'sticky'}
								onchange={() => applyPreset('sticky')}
							/>
							<span>
								<span class="font-medium text-foreground">Stays online until toggled off</span>
								<span class="block text-xs text-muted-foreground">
									Presence sticks once they mark themselves available — no automatic timeout.
								</span>
							</span>
						</label>
					</div>

					<details class="rounded-md border border-border/60 bg-muted/20 p-2">
						<summary class="cursor-pointer text-xs font-medium text-muted-foreground">
							Advanced{presetMode === 'custom' ? ' (custom)' : ''}
						</summary>
						<div class="mt-2 grid gap-2 sm:grid-cols-3">
							<div class="space-y-1 sm:col-span-3">
								<label
									for="adv-liveness"
									class="text-xs font-medium uppercase tracking-wide text-muted-foreground"
								>
									Liveness source
								</label>
								<Select.Root
									type="single"
									value={availability.liveness_source}
									onValueChange={(v) =>
										(availability = {
											...availability,
											liveness_source: (v ?? 'session') as AvailabilityConfig['liveness_source']
										})}
								>
									<Select.Trigger id="adv-liveness" class="h-8 w-full">
										{availability.liveness_source}
									</Select.Trigger>
									<Select.Content>
										<Select.Item value="session" label="session" />
										<Select.Item value="none" label="none" />
										<Select.Item value="external" label="external" />
									</Select.Content>
								</Select.Root>
							</div>
							<div class="space-y-1">
								<label
									for="adv-ttl"
									class="text-xs font-medium uppercase tracking-wide text-muted-foreground"
								>
									TTL secs
								</label>
								<Input
									id="adv-ttl"
									type="number"
									min="0"
									value={availability.ttl_secs}
									oninput={(e) =>
										(availability = {
											...availability,
											ttl_secs: Number((e.currentTarget as HTMLInputElement).value) || 0
										})}
								/>
							</div>
							<div class="space-y-1">
								<label
									for="adv-grace"
									class="text-xs font-medium uppercase tracking-wide text-muted-foreground"
								>
									Grace secs
								</label>
								<Input
									id="adv-grace"
									type="number"
									min="0"
									value={availability.grace_secs}
									oninput={(e) =>
										(availability = {
											...availability,
											grace_secs: Number((e.currentTarget as HTMLInputElement).value) || 0
										})}
								/>
							</div>
						</div>
					</details>
				</div>

				<!-- Capabilities -->
				<div class="space-y-1">
					<span class="text-sm font-medium uppercase tracking-wide text-muted-foreground">
						Capabilities
					</span>
					<CapsEditor value={caps} onchange={(next) => (caps = next)} />
				</div>

				<div class="flex gap-2 pt-1">
					<Button type="submit" disabled={!canSubmit} class="flex-1">
						{#if submitting}
							{isEdit ? 'Saving…' : 'Enrolling…'}
						{:else}
							{isEdit ? 'Save changes' : 'Enroll member'}
						{/if}
					</Button>
					<SheetClose>
						<Button type="button" variant="outline">Cancel</Button>
					</SheetClose>
				</div>
			</form>
		</div>
	</SheetContent>
</Sheet.Root>
