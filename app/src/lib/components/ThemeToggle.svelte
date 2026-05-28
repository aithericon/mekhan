<script lang="ts">
	import Sun from '@lucide/svelte/icons/sun';
	import Moon from '@lucide/svelte/icons/moon';
	import { setMode, resetMode, userPrefersMode } from 'mode-watcher';
	import { Button } from '$lib/components/ui/button';
	import {
		DropdownMenu,
		DropdownMenuTrigger,
		DropdownMenuContent,
		DropdownMenuItem
	} from '$lib/components/ui/dropdown-menu';
</script>

<DropdownMenu>
	<DropdownMenuTrigger
		data-testid="theme-toggle"
		title="Switch theme"
		class="relative inline-flex size-8 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground data-[state=open]:bg-accent"
	>
		<Sun
			class="size-4 scale-100 rotate-0 transition-all dark:scale-0 dark:-rotate-90"
		/>
		<Moon
			class="absolute size-4 scale-0 rotate-90 transition-all dark:scale-100 dark:rotate-0"
		/>
		<span class="sr-only">Toggle theme</span>
	</DropdownMenuTrigger>
	<DropdownMenuContent align="end" class="w-36">
		<DropdownMenuItem
			onclick={() => setMode('light')}
			data-testid="theme-light"
			class={userPrefersMode.current === 'light' ? 'font-medium text-foreground' : ''}
		>
			<Sun class="size-4" />
			Light
		</DropdownMenuItem>
		<DropdownMenuItem
			onclick={() => setMode('dark')}
			data-testid="theme-dark"
			class={userPrefersMode.current === 'dark' ? 'font-medium text-foreground' : ''}
		>
			<Moon class="size-4" />
			Dark
		</DropdownMenuItem>
		<DropdownMenuItem
			onclick={() => resetMode()}
			data-testid="theme-system"
			class={userPrefersMode.current === 'system' ? 'font-medium text-foreground' : ''}
		>
			System
		</DropdownMenuItem>
	</DropdownMenuContent>
</DropdownMenu>
