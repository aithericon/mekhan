<script lang="ts">
	import { Command as CommandPrimitive, Dialog as DialogPrimitive } from 'bits-ui';
	import type { Snippet } from 'svelte';
	import Command from './command.svelte';
	import * as Dialog from '$lib/components/ui/dialog/index.js';
	import { cn, type WithoutChildrenOrChild } from '$lib/utils.js';

	let {
		open = $bindable(false),
		value = $bindable(''),
		title = 'Command Palette',
		description = 'Search for a command to run...',
		class: className,
		portalProps,
		children,
		showCloseButton = true,
		...restProps
	}: WithoutChildrenOrChild<CommandPrimitive.RootProps> &
		Pick<DialogPrimitive.RootProps, 'open'> & {
			portalProps?: DialogPrimitive.PortalProps;
			title?: string;
			description?: string;
			showCloseButton?: boolean;
			children: Snippet;
		} = $props();
</script>

<Dialog.Root bind:open>
	<Dialog.Content class={cn('overflow-hidden p-0', className)} {portalProps} {showCloseButton}>
		<Dialog.Header class="sr-only">
			<Dialog.Title>{title}</Dialog.Title>
			<Dialog.Description>{description}</Dialog.Description>
		</Dialog.Header>
		<Command
			class="[&_[data-command-group-heading]]:text-muted-foreground [&_[data-command-group]]:px-2 [&_[data-command-input-wrapper]]:h-12 [&_[data-command-input]]:h-12 [&_[data-command-item]]:px-2 [&_[data-command-item]]:py-3"
			bind:value
			{...restProps}
		>
			{@render children?.()}
		</Command>
	</Dialog.Content>
</Dialog.Root>
