<script lang="ts">
	import { Button } from '$lib/components/ui/button';
	import {
		Card,
		CardHeader,
		CardTitle,
		CardDescription,
		CardContent
	} from '$lib/components/ui/card';
	import { Input } from '$lib/components/ui/input';
	import { CopyButton } from '$lib/components/ui/copy-button';
	import {
		Sheet,
		SheetContent,
		SheetTitle,
		SheetDescription,
		SheetClose
	} from '$lib/components/ui/sheet';
	import { toast } from 'svelte-sonner';
	import KeyRound from '@lucide/svelte/icons/key-round';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import TriangleAlert from '@lucide/svelte/icons/triangle-alert';
	import {
		listAccessTokens,
		createAccessToken,
		revokeAccessToken,
		type TokenSummary,
		type CreatedToken
	} from '$lib/api/client';

	// `null` from the API ⇒ the server has no broker configured (503): the
	// whole section is hidden rather than shown broken.
	let disabled = $state(false);
	let loading = $state(true);
	let tokens = $state<TokenSummary[]>([]);

	let name = $state('');
	let description = $state('');
	let expiresAt = $state(''); // YYYY-MM-DD from <input type="date">
	let creating = $state(false);
	let revoking = $state<string | null>(null);

	// The freshly-minted PAT — shown exactly once, in a modal.
	let revealed = $state<CreatedToken | null>(null);

	async function load() {
		loading = true;
		try {
			const res = await listAccessTokens();
			if (res === null) {
				disabled = true;
				return;
			}
			tokens = res;
		} catch (e) {
			toast.error(`Couldn't load tokens: ${e instanceof Error ? e.message : e}`);
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		load();
	});

	async function create() {
		if (creating) return;
		const trimmed = name.trim();
		if (!trimmed) {
			toast.error('Give the token a name.');
			return;
		}
		creating = true;
		try {
			const created = await createAccessToken({
				name: trimmed,
				description: description.trim() || undefined,
				// Date input is a calendar day; treat it as end-of-day UTC.
				expires_at: expiresAt ? `${expiresAt}T23:59:59Z` : undefined
			});
			revealed = created;
			name = '';
			description = '';
			expiresAt = '';
			toast.success('Token created.');
			await load();
		} catch (e) {
			toast.error(`Create failed: ${e instanceof Error ? e.message : e}`);
		} finally {
			creating = false;
		}
	}

	async function revoke(token: TokenSummary) {
		if (revoking) return;
		if (!confirm(`Revoke "${token.name}"? Anything using this token stops working immediately.`))
			return;
		revoking = token.id;
		try {
			await revokeAccessToken(token.id);
			toast.success('Token revoked.');
			await load();
		} catch (e) {
			toast.error(`Revoke failed: ${e instanceof Error ? e.message : e}`);
		} finally {
			revoking = null;
		}
	}

	function fmt(ts: string | null | undefined): string {
		if (!ts) return '—';
		const d = new Date(ts);
		return Number.isNaN(d.getTime()) ? ts : d.toLocaleString();
	}
</script>

{#if !disabled}
	<Card class="mt-6" data-testid="access-tokens">
		<CardHeader>
			<CardTitle class="flex items-center gap-2">
				<KeyRound class="size-4" />
				Access tokens
			</CardTitle>
			<CardDescription>
				Personal tokens for non-interactive use — e.g.
				<code class="rounded bg-muted px-1 py-0.5 font-mono text-xs">MEKHAN_CLI_TOKEN</code>
				for <code class="rounded bg-muted px-1 py-0.5 font-mono text-xs">mekhan apply</code>.
				The secret is shown once, on creation.
			</CardDescription>
		</CardHeader>

		<CardContent class="space-y-6">
			<!-- Existing tokens -->
			<div class="space-y-2">
				{#if loading}
					<p class="text-sm text-muted-foreground">Loading…</p>
				{:else if tokens.length === 0}
					<p class="text-sm text-muted-foreground" data-testid="tokens-empty">
						No tokens yet.
					</p>
				{:else}
					{#each tokens as token (token.id)}
						<div
							class="flex items-start justify-between gap-3 rounded-lg border border-border p-3"
						>
							<div class="min-w-0 space-y-0.5">
								<p class="truncate text-sm font-medium text-foreground">{token.name}</p>
								{#if token.description}
									<p class="truncate text-xs text-muted-foreground">
										{token.description}
									</p>
								{/if}
								<p class="text-xs text-muted-foreground">
									Created {fmt(token.created_at)} · Expires {fmt(token.expires_at)}
								</p>
							</div>
							<Button
								variant="destructive"
								size="sm"
								onclick={() => revoke(token)}
								disabled={revoking === token.id}
							>
								<Trash2 class="size-3.5" />
								{revoking === token.id ? 'Revoking…' : 'Revoke'}
							</Button>
						</div>
					{/each}
				{/if}
			</div>

			<!-- Create -->
			<form
				class="space-y-3 border-t border-border pt-4"
				onsubmit={(e) => {
					e.preventDefault();
					create();
				}}
			>
				<div class="space-y-1">
					<label
						for="token-name"
						class="text-xs font-medium uppercase tracking-wide text-muted-foreground"
					>
						Name
					</label>
					<Input id="token-name" bind:value={name} placeholder="e.g. ci-deploy" />
				</div>
				<div class="grid gap-3 sm:grid-cols-2">
					<div class="space-y-1">
						<label
							for="token-desc"
							class="text-xs font-medium uppercase tracking-wide text-muted-foreground"
						>
							Description <span class="normal-case">(optional)</span>
						</label>
						<Input
							id="token-desc"
							bind:value={description}
							placeholder="What is this for?"
						/>
					</div>
					<div class="space-y-1">
						<label
							for="token-exp"
							class="text-xs font-medium uppercase tracking-wide text-muted-foreground"
						>
							Expires <span class="normal-case">(optional)</span>
						</label>
						<Input id="token-exp" type="date" bind:value={expiresAt} />
					</div>
				</div>
				<Button type="submit" disabled={creating} data-testid="create-token">
					{creating ? 'Creating…' : 'Create token'}
				</Button>
			</form>
		</CardContent>
	</Card>
{/if}

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
				<SheetDescription class="text-xs text-muted-foreground">
					Copy this now — it is not stored and will never be shown again.
				</SheetDescription>
			</div>

			<div
				class="flex items-start gap-2 rounded-md border border-amber-500/40 bg-amber-500/10 p-2 text-xs text-amber-700 dark:text-amber-400"
			>
				<TriangleAlert class="mt-0.5 size-3.5 shrink-0" />
				<span>Treat it like a password. Anyone with it can act as you in automation.</span>
			</div>

			{#if revealed}
				<div class="flex items-center gap-2">
					<code
						class="flex-1 break-all rounded bg-muted px-2 py-1.5 font-mono text-xs text-foreground"
						data-testid="token-secret"
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
