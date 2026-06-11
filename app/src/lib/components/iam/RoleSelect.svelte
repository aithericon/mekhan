<!--
  RoleSelect — a thin shadcn (bits-ui) Select wrapper for picking a role.
  Controlled: pass the current `value` reactively and handle `onSelect`
  (mirrors the folder-parent Select idiom — no two-way bind needed). Labels
  are capitalized via CSS so callers pass plain role strings.
-->
<script lang="ts">
	import * as Select from '$lib/components/ui/select';

	let {
		value,
		roles,
		onSelect,
		disabled = false,
		size = 'sm',
		title,
		testid,
		ariaLabel = 'Role',
		class: className
	}: {
		value: string;
		/** Role options, in display order. */
		roles: readonly string[];
		onSelect: (role: string) => void;
		disabled?: boolean;
		size?: 'sm' | 'default';
		title?: string;
		testid?: string;
		ariaLabel?: string;
		class?: string;
	} = $props();
</script>

<Select.Root
	type="single"
	{value}
	{disabled}
	onValueChange={(v) => {
		if (v && v !== value) onSelect(v);
	}}
>
	<Select.Trigger
		{size}
		class={className ?? 'w-32'}
		data-testid={testid}
		aria-label={ariaLabel}
		{title}
	>
		<span class="truncate capitalize">{value}</span>
	</Select.Trigger>
	<Select.Content>
		{#each roles as r (r)}
			<Select.Item value={r} label={r} class="capitalize" />
		{/each}
	</Select.Content>
</Select.Root>
