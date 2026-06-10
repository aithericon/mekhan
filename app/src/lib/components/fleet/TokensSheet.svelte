<script lang="ts">
	// Enrollment-tokens sheet — the registration-token sub-list that used to sit
	// at the bottom of RunnerList, lifted into a drawer opened from the Machines
	// toolbar's "Tokens" button. Lists the workspace's runner registration tokens
	// (group, reusable vs 1-shot · uses, max uses, created/expires) with revoke.
	// Worker tokens have no list API — they are mint-only (see the footnote).
	import {
		Sheet,
		SheetContent,
		SheetTitle,
		SheetDescription,
		SheetClose
	} from '$lib/components/ui/sheet';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import { toast } from 'svelte-sonner';
	import KeyRound from '@lucide/svelte/icons/key-round';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import {
		listRegistrationTokens,
		revokeRegistrationToken,
		type RegistrationTokenSummary
	} from '$lib/api/runners';
	import { fmtDate } from './format';

	type Props = {
		open: boolean;
	};
	let { open = $bindable() }: Props = $props();

	let tokens = $state<RegistrationTokenSummary[]>([]);
	let loading = $state(false);
	let error = $state<string | null>(null);
	let revokingToken = $state<string | null>(null);

	async function load() {
		loading = true;
		error = null;
		try {
			const page = await listRegistrationTokens({ perPage: 200 });
			tokens = page.items;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load tokens';
			tokens = [];
		} finally {
			loading = false;
		}
	}

	// Refresh the list each time the sheet opens.
	$effect(() => {
		if (open) void load();
	});

	async function handleRevokeToken(token: RegistrationTokenSummary) {
		if (revokingToken) return;
		if (!confirm("Revoke this registration token? Runners that haven't enrolled yet won't be able to use it.")) return;
		revokingToken = token.id;
		try {
			await revokeRegistrationToken(token.id);
			toast.success('Token revoked.');
			await load();
		} catch (e) {
			toast.error(`Revoke failed: ${e instanceof Error ? e.message : e}`);
		} finally {
			revokingToken = null;
		}
	}
</script>

<Sheet.Root
	{open}
	onOpenChange={(o: boolean) => {
		if (!o) open = false;
	}}
>
	<SheetContent class="w-[520px] overflow-y-auto sm:max-w-[520px]">
		<div class="space-y-4 p-2" data-testid="tokens-sheet">
			<div class="space-y-1">
				<SheetTitle class="flex items-center gap-2 text-lg font-semibold">
					<KeyRound class="size-4" />
					Enrollment tokens
				</SheetTitle>
				<SheetDescription class="text-sm text-muted-foreground">
					Runner registration tokens minted in this workspace. The token secret is shown once at
					mint and never re-served.
				</SheetDescription>
			</div>

			{#if error}
				<div class="rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
					{error}
				</div>
			{:else if loading}
				<p class="py-8 text-center text-sm text-muted-foreground">Loading…</p>
			{:else if tokens.length === 0}
				<p class="py-8 text-center text-sm text-muted-foreground">
					No registration tokens minted yet.
				</p>
			{:else}
				<div class="space-y-2">
					{#each tokens as token (token.id)}
						<div
							class="group flex items-center justify-between rounded-lg border border-border bg-card px-4 py-3 transition-colors hover:bg-accent/40"
							data-testid="token-item-{token.id}"
						>
							<div class="min-w-0 space-y-0.5">
								<div class="flex flex-wrap items-center gap-2">
									{#if token.group}
										<Badge variant="secondary" class="text-sm">{token.group}</Badge>
									{/if}
									<Badge variant="outline" class="text-sm">
										{token.reusable ? 'reusable' : `1-shot · ${token.uses} used`}
									</Badge>
									{#if token.max_uses}
										<span class="text-sm text-muted-foreground">max {token.max_uses}</span>
									{/if}
								</div>
								<p class="text-sm text-muted-foreground">
									Created {fmtDate(token.created_at)}
									{#if token.expires_at}· Expires {fmtDate(token.expires_at)}{/if}
								</p>
							</div>
							<Button
								variant="ghost"
								size="sm"
								class="opacity-0 transition-opacity group-hover:opacity-100 text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
								onclick={() => handleRevokeToken(token)}
								disabled={revokingToken === token.id}
							>
								<Trash2 class="size-3.5" />
								{revokingToken === token.id ? 'Revoking…' : 'Revoke'}
							</Button>
						</div>
					{/each}
				</div>
			{/if}

			<p class="text-xs text-muted-foreground/70">
				Worker tokens are mint-only and not listed.
			</p>

			<SheetClose>
				<Button variant="outline" class="w-full">Close</Button>
			</SheetClose>
		</div>
	</SheetContent>
</Sheet.Root>
