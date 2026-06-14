<script lang="ts">
	import { onMount } from 'svelte';
	import Check from '@lucide/svelte/icons/check';
	import ChevronsUpDown from '@lucide/svelte/icons/chevrons-up-down';
	import UserCog from '@lucide/svelte/icons/user-cog';
	import { Button } from '$lib/components/ui/button';
	import {
		DropdownMenu,
		DropdownMenuTrigger,
		DropdownMenuContent,
		DropdownMenuItem,
		DropdownMenuSeparator
	} from '$lib/components/ui/dropdown-menu';
	import { devIdentity } from '$lib/dev/identity.svelte';

	onMount(() => {
		devIdentity.load();
	});

	async function switchTo(subject: string) {
		try {
			await devIdentity.switchTo(subject);
		} catch (err) {
			console.error('failed to switch dev identity', err);
		}
	}

	const active = $derived(devIdentity.active);
	const list = $derived(devIdentity.identities);
</script>

<!-- Dev-only: the roster is empty under real auth modes, so this renders
     nothing outside `dev_noop`. -->
{#if devIdentity.enabled}
	<DropdownMenu>
		<DropdownMenuTrigger>
			<Button
				variant="ghost"
				size="sm"
				class="gap-1.5 text-amber-600 dark:text-amber-400"
				data-testid="dev-identity-picker-trigger"
				title="Switch acting dev user (dev_noop)"
			>
				<UserCog class="size-3.5" />
				<span class="max-w-[14ch] truncate text-sm">
					{active?.display_name ?? active?.subject ?? '…'}
				</span>
				<ChevronsUpDown class="size-3 opacity-60" />
			</Button>
		</DropdownMenuTrigger>
		<DropdownMenuContent class="min-w-[16rem]" align="end">
			<div class="px-2 py-1.5 text-xs text-muted-foreground">Act as (dev only)</div>
			<DropdownMenuSeparator />
			{#each list as id (id.subject)}
				<DropdownMenuItem
					onclick={() => switchTo(id.subject)}
					data-testid={`dev-identity-option-${id.subject}`}
				>
					<div class="flex w-full items-center gap-2">
						<div class="min-w-0 flex-1">
							<div class="truncate text-sm">{id.display_name ?? id.subject}</div>
							<div class="truncate text-xs text-muted-foreground">
								{id.email ?? id.subject}
							</div>
						</div>
						{#if id.active}
							<Check class="size-4 text-foreground" />
						{/if}
					</div>
				</DropdownMenuItem>
			{/each}
		</DropdownMenuContent>
	</DropdownMenu>
{/if}
