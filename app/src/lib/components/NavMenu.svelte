<script lang="ts">
	import { Button } from '$lib/components/ui/button';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';

	export type NavMenuItem = { href: string; label: string; testid?: string; desc?: string };

	type Props = {
		label: string;
		items: NavMenuItem[];
		testid?: string;
		/** Dim the trigger to match low-priority (engine/admin) nav. */
		muted?: boolean;
	};

	let { label, items, testid, muted = false }: Props = $props();

	// Opens on hover (the panel is a DOM child of the wrapper, so mouseleave only
	// fires when the pointer leaves both trigger and panel — no portal hover-gap)
	// and also toggles on click / closes on Escape for keyboard + touch.
	let open = $state(false);
	let closeTimer: ReturnType<typeof setTimeout> | null = null;

	function openNow() {
		if (closeTimer) {
			clearTimeout(closeTimer);
			closeTimer = null;
		}
		open = true;
	}

	// Small grace period so a brief exit (e.g. crossing a sub-pixel gap) doesn't
	// flicker the panel shut.
	function scheduleClose() {
		if (closeTimer) clearTimeout(closeTimer);
		closeTimer = setTimeout(() => (open = false), 120);
	}
</script>

<div
	class="relative"
	role="presentation"
	onmouseenter={openNow}
	onmouseleave={scheduleClose}
	onkeydown={(e) => {
		if (e.key === 'Escape') open = false;
	}}
	data-testid={testid}
>
	<Button
		variant="ghost"
		size="sm"
		class="gap-1 data-[open=true]:bg-accent data-[open=true]:text-foreground {muted
			? 'text-muted-foreground'
			: ''}"
		data-open={open}
		aria-haspopup="menu"
		aria-expanded={open}
		onclick={() => (open = !open)}
	>
		{label}
		<ChevronDown class="size-3.5 transition-transform duration-150 {open ? 'rotate-180' : ''}" />
	</Button>

	{#if open}
		<!-- pt-1 keeps the visible panel detached from the trigger while leaving a
		     contiguous hover bridge (it's inside the wrapper). -->
		<div class="absolute top-full left-0 z-50 pt-1" role="menu" aria-label={label}>
			<div class="min-w-56 rounded-md border border-border bg-popover p-1 shadow-md">
				{#each items as it (it.href)}
					<a
						href={it.href}
						data-testid={it.testid}
						role="menuitem"
						class="flex flex-col gap-0.5 rounded-sm px-2 py-1.5 transition-colors hover:bg-accent"
						onclick={() => (open = false)}
					>
						<span class="text-sm text-foreground">{it.label}</span>
						{#if it.desc}
							<span class="text-xs text-muted-foreground">{it.desc}</span>
						{/if}
					</a>
				{/each}
			</div>
		</div>
	{/if}
</div>
