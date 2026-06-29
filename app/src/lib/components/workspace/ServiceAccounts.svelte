<!--
  ServiceAccounts — workspace settings card for NON-human, workspace-owned API
  principals. Unlike a member's personal access token, a service account is
  owned by the workspace itself: it carries a fixed workspace role and survives
  member offboarding (it dies only when disabled or its token is revoked).

  Management is human-Admin-only and enforced server-side; this component only
  hides/disables affordances when `canAdmin` is false. Mirrors the Members /
  Invites cards on the workspace settings page and the one-time secret reveal of
  the profile AccessTokens card.
-->
<script lang="ts">
	import { Button } from '$lib/components/ui/button';
	import {
		Card,
		CardHeader,
		CardTitle,
		CardContent,
		CardDescription
	} from '$lib/components/ui/card';
	import { Input } from '$lib/components/ui/input';
	import { Badge } from '$lib/components/ui/badge';
	import { CopyButton } from '$lib/components/ui/copy-button';
	import {
		Sheet,
		SheetContent,
		SheetTitle,
		SheetDescription,
		SheetClose
	} from '$lib/components/ui/sheet';
	import RoleSelect from '$lib/components/iam/RoleSelect.svelte';
	import { toast } from 'svelte-sonner';
	import Bot from '@lucide/svelte/icons/bot';
	import KeyRound from '@lucide/svelte/icons/key-round';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import TriangleAlert from '@lucide/svelte/icons/triangle-alert';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import {
		listServiceAccounts,
		createServiceAccount,
		patchServiceAccount,
		deleteServiceAccount,
		listServiceAccountTokens,
		createServiceAccountToken,
		revokeServiceAccountToken,
		type ServiceAccountSummary,
		type ServiceAccountTokenSummary,
		type CreatedServiceAccountToken
	} from '$lib/api/service-accounts';

	let { workspaceId, canAdmin }: { workspaceId: string; canAdmin: boolean } = $props();

	// A service account may NEVER be a workspace owner — narrower than the
	// member/invite role set.
	const SA_ROLES = ['viewer', 'editor', 'admin'] as const;

	let serviceAccounts = $state<ServiceAccountSummary[]>([]);
	let loading = $state(true);

	// Create-SA form
	let saName = $state('');
	let saRole = $state<'viewer' | 'editor' | 'admin'>('editor');
	let creating = $state(false);

	// Per-SA token sub-list (lazily loaded on expand)
	let expanded = $state<string | null>(null);
	let tokensBySa = $state<Record<string, ServiceAccountTokenSummary[]>>({});
	let tokensLoading = $state<string | null>(null);

	// Generate-token form (scoped to the expanded SA)
	let tokenName = $state('');
	let tokenExpires = $state(''); // YYYY-MM-DD from <input type="date">
	let minting = $state(false);

	// Freshly-minted token — shown exactly once, in a modal.
	let revealed = $state<CreatedServiceAccountToken | null>(null);

	// Generic per-row busy marker (disable/delete/revoke).
	let busy = $state<string | null>(null);

	async function load() {
		loading = true;
		try {
			// Management is Admin-only; tolerate a 403 for non-admins.
			serviceAccounts = await listServiceAccounts(workspaceId).catch(() => []);
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		if (workspaceId) load();
	});

	async function createSa(e: Event) {
		e.preventDefault();
		if (creating) return;
		const name = saName.trim();
		if (!name) {
			toast.error('Give the service account a name.');
			return;
		}
		creating = true;
		try {
			await createServiceAccount(workspaceId, { name, role: saRole });
			saName = '';
			saRole = 'editor';
			toast.success('Service account created.');
			await load();
		} catch (err) {
			toast.error(`Create failed: ${err instanceof Error ? err.message : err}`);
		} finally {
			creating = false;
		}
	}

	async function toggleExpand(saId: string) {
		if (expanded === saId) {
			expanded = null;
			return;
		}
		expanded = saId;
		tokenName = '';
		tokenExpires = '';
		await loadTokens(saId);
	}

	async function loadTokens(saId: string) {
		tokensLoading = saId;
		try {
			tokensBySa = { ...tokensBySa, [saId]: await listServiceAccountTokens(workspaceId, saId) };
		} catch (err) {
			toast.error(`Couldn't load tokens: ${err instanceof Error ? err.message : err}`);
		} finally {
			tokensLoading = null;
		}
	}

	async function mintToken(saId: string) {
		if (minting) return;
		const name = tokenName.trim();
		if (!name) {
			toast.error('Give the token a name.');
			return;
		}
		minting = true;
		try {
			const created = await createServiceAccountToken(workspaceId, saId, {
				name,
				// Date input is a calendar day; treat it as end-of-day UTC.
				expires_at: tokenExpires ? `${tokenExpires}T23:59:59Z` : undefined
			});
			revealed = created;
			tokenName = '';
			tokenExpires = '';
			toast.success('Token created.');
			await loadTokens(saId);
		} catch (err) {
			toast.error(`Create failed: ${err instanceof Error ? err.message : err}`);
		} finally {
			minting = false;
		}
	}

	async function revokeToken(saId: string, token: ServiceAccountTokenSummary) {
		if (busy) return;
		if (
			!confirm(`Revoke "${token.name}"? Anything using this token stops working immediately.`)
		)
			return;
		busy = token.id;
		try {
			await revokeServiceAccountToken(workspaceId, saId, token.id);
			toast.success('Token revoked.');
			await loadTokens(saId);
		} catch (err) {
			toast.error(`Revoke failed: ${err instanceof Error ? err.message : err}`);
		} finally {
			busy = null;
		}
	}

	async function setDisabled(sa: ServiceAccountSummary, disabled: boolean) {
		if (busy) return;
		const verb = disabled ? 'Disable' : 'Re-enable';
		if (disabled && !confirm(`${verb} "${sa.name}"? Its tokens stop authenticating immediately.`))
			return;
		busy = sa.id;
		try {
			await patchServiceAccount(workspaceId, sa.id, { disabled });
			toast.success(`Service account ${disabled ? 'disabled' : 're-enabled'}.`);
			await load();
		} catch (err) {
			toast.error(`${verb} failed: ${err instanceof Error ? err.message : err}`);
		} finally {
			busy = null;
		}
	}

	async function deleteSa(sa: ServiceAccountSummary) {
		if (busy) return;
		if (!confirm(`Delete "${sa.name}"? This permanently removes it and all its tokens.`)) return;
		busy = sa.id;
		try {
			await deleteServiceAccount(workspaceId, sa.id);
			if (expanded === sa.id) expanded = null;
			toast.success('Service account deleted.');
			await load();
		} catch (err) {
			toast.error(`Delete failed: ${err instanceof Error ? err.message : err}`);
		} finally {
			busy = null;
		}
	}

	function fmt(ts: string | null | undefined): string {
		if (!ts) return '—';
		const d = new Date(ts);
		return Number.isNaN(d.getTime()) ? ts : d.toLocaleString();
	}
</script>

<Card data-testid="service-accounts-card">
	<CardHeader>
		<CardTitle class="flex items-center gap-2">
			<Bot class="size-4" />
			Service accounts
		</CardTitle>
		<CardDescription>
			Workspace-owned API principals for CI/automation. Unlike a personal token,
			a service account survives member offboarding — it dies only when disabled
			or its token is revoked.
		</CardDescription>
	</CardHeader>
	<CardContent class="space-y-4">
		{#if canAdmin}
			<form onsubmit={createSa} class="space-y-2">
				<div class="flex gap-2">
					<Input
						placeholder="e.g. ci-deploy"
						bind:value={saName}
						data-testid="input-sa-name"
						class="flex-1"
					/>
					<RoleSelect
						value={saRole}
						roles={SA_ROLES}
						onSelect={(r) => (saRole = r as typeof saRole)}
						size="default"
						testid="select-sa-role"
						ariaLabel="Service account role"
					/>
					<Button type="submit" disabled={creating} data-testid="btn-create-sa">
						<Bot class="size-4" />
						Create
					</Button>
				</div>
			</form>
		{/if}

		{#if loading}
			<p class="text-sm text-muted-foreground">Loading…</p>
		{:else if serviceAccounts.length === 0}
			<p class="text-sm text-muted-foreground" data-testid="service-accounts-empty">
				No service accounts yet.
			</p>
		{:else}
			<ul class="divide-y divide-border rounded-md border border-border">
				{#each serviceAccounts as sa (sa.id)}
					<li class="text-sm" data-testid={`service-account-row-${sa.id}`}>
						<div class="flex items-center justify-between gap-3 px-3 py-2">
							<button
								type="button"
								class="flex min-w-0 flex-1 items-center gap-2 text-left"
								onclick={() => toggleExpand(sa.id)}
								data-testid={`btn-expand-sa-${sa.id}`}
								aria-expanded={expanded === sa.id}
							>
								<ChevronRight
									class={`size-3.5 shrink-0 text-muted-foreground transition-transform ${
										expanded === sa.id ? 'rotate-90' : ''
									}`}
								/>
								<span class="truncate font-medium text-foreground">{sa.name}</span>
								{#if sa.disabled_at}
									<Badge variant="secondary" data-testid={`sa-disabled-${sa.id}`}>disabled</Badge>
								{/if}
							</button>
							<Badge variant="outline" class="capitalize">{sa.role}</Badge>
							{#if canAdmin}
								<button
									type="button"
									class="text-xs text-muted-foreground hover:text-foreground disabled:opacity-40"
									onclick={() => setDisabled(sa, !sa.disabled_at)}
									disabled={busy === sa.id}
									data-testid={`btn-toggle-sa-${sa.id}`}
								>
									{sa.disabled_at ? 'Enable' : 'Disable'}
								</button>
								<button
									type="button"
									class="text-muted-foreground hover:text-destructive disabled:opacity-40"
									onclick={() => deleteSa(sa)}
									disabled={busy === sa.id}
									title="Delete service account"
									aria-label="Delete service account"
									data-testid={`btn-delete-sa-${sa.id}`}
								>
									<Trash2 class="size-3.5" />
								</button>
							{/if}
						</div>

						{#if expanded === sa.id}
							<div class="space-y-3 border-t border-border bg-muted/30 px-3 py-3">
								<p class="text-xs text-muted-foreground">
									Created {fmt(sa.created_at)}
								</p>

								<!-- Token list -->
								{#if tokensLoading === sa.id}
									<p class="text-sm text-muted-foreground">Loading tokens…</p>
								{:else if (tokensBySa[sa.id] ?? []).length === 0}
									<p class="text-sm text-muted-foreground" data-testid={`sa-tokens-empty-${sa.id}`}>
										No tokens yet.
									</p>
								{:else}
									<ul class="space-y-1">
										{#each tokensBySa[sa.id] ?? [] as token (token.id)}
											<li
												class="flex items-center justify-between gap-3 rounded border border-border bg-card px-2 py-1.5"
												data-testid={`sa-token-row-${token.id}`}
											>
												<div class="min-w-0 space-y-0.5">
													<p class="truncate font-medium text-foreground">{token.name}</p>
													<p class="text-xs text-muted-foreground">
														Created {fmt(token.created_at)} · Expires {fmt(token.expires_at)} · Last
														used {fmt(token.last_used_at)}
													</p>
												</div>
												{#if canAdmin}
													<Button
														variant="destructive"
														size="sm"
														onclick={() => revokeToken(sa.id, token)}
														disabled={busy === token.id}
														data-testid={`btn-revoke-sa-token-${token.id}`}
													>
														<Trash2 class="size-3.5" />
														{busy === token.id ? 'Revoking…' : 'Revoke'}
													</Button>
												{/if}
											</li>
										{/each}
									</ul>
								{/if}

								<!-- Generate token -->
								{#if canAdmin}
									<form
										class="flex flex-wrap items-end gap-2 border-t border-border pt-3"
										onsubmit={(e) => {
											e.preventDefault();
											mintToken(sa.id);
										}}
									>
										<div class="flex-1 space-y-1">
											<label
												for={`sa-token-name-${sa.id}`}
												class="text-xs font-medium uppercase tracking-wide text-muted-foreground"
											>
												Token name
											</label>
											<Input
												id={`sa-token-name-${sa.id}`}
												bind:value={tokenName}
												placeholder="e.g. nightly-deploy"
												data-testid={`input-sa-token-name-${sa.id}`}
											/>
										</div>
										<div class="space-y-1">
											<label
												for={`sa-token-exp-${sa.id}`}
												class="text-xs font-medium uppercase tracking-wide text-muted-foreground"
											>
												Expires <span class="normal-case">(optional)</span>
											</label>
											<Input
												id={`sa-token-exp-${sa.id}`}
												type="date"
												bind:value={tokenExpires}
											/>
										</div>
										<Button
											type="submit"
											disabled={minting}
											data-testid={`btn-generate-sa-token-${sa.id}`}
										>
											<KeyRound class="size-4" />
											{minting ? 'Generating…' : 'Generate token'}
										</Button>
									</form>
								{/if}
							</div>
						{/if}
					</li>
				{/each}
			</ul>
		{/if}
	</CardContent>
</Card>

<!-- One-time secret reveal -->
<Sheet.Root
	open={revealed !== null}
	onOpenChange={(o: boolean) => {
		if (!o) revealed = null;
	}}
>
	<SheetContent class="w-[480px] sm:max-w-[480px]">
		<div class="space-y-4 p-2">
			<div class="space-y-1">
				<SheetTitle class="flex items-center gap-2 text-lg font-semibold">
					<KeyRound class="size-4" />
					{revealed?.name}
				</SheetTitle>
				<SheetDescription class="text-sm text-muted-foreground">
					Copy this now — it is not stored and will never be shown again.
				</SheetDescription>
			</div>

			<div
				class="flex items-start gap-2 rounded-md border border-amber-500/40 bg-amber-500/10 p-2 text-sm text-amber-700 dark:text-amber-400"
			>
				<TriangleAlert class="mt-0.5 size-3.5 shrink-0" />
				<span>
					Treat it like a password. Anyone with it can act as this service account in automation.
				</span>
			</div>

			{#if revealed}
				<div class="flex items-center gap-2">
					<code
						class="flex-1 break-all rounded bg-muted px-2 py-1.5 font-mono text-sm text-foreground"
						data-testid="sa-token-secret"
					>
						{revealed.secret}
					</code>
					<CopyButton text={revealed.secret} />
				</div>
			{/if}

			<SheetClose>
				<Button variant="outline" class="w-full">Done</Button>
			</SheetClose>
		</div>
	</SheetContent>
</Sheet.Root>
