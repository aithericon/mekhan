<script lang="ts">
	// Read-only "Outputs" / "Inputs" preview for nodes whose ports are derived
	// from inner config (HumanTask from task fields, Decision from branches,
	// ParallelSplit/Join, Loop, Scope). Mirrors what the compiler will see at
	// publish time via `outputPortsFor` / `inputPortsFor`.
	//
	// Editable ports (Start.initial, AutomatedStep.output, End.terminal) use
	// `PortsSection.svelte` directly instead.

	import type { components } from '$lib/api/schema';

	type Port = components['schemas']['Port'];

	type Props = {
		ports: Port[];
		title?: string;
		derivedFrom?: string;
	};

	let { ports, title = 'Outputs', derivedFrom = 'Derived' }: Props = $props();
</script>

<div class="space-y-2">
	<div class="flex items-center justify-between">
		<span class="text-xs font-medium text-muted-foreground">{title}</span>
		<span class="text-[10px] uppercase tracking-wide text-muted-foreground/80">{derivedFrom}</span>
	</div>

	{#if ports.length === 0}
		<p class="rounded-md border border-dashed border-border/50 p-2 text-[11px] text-muted-foreground">
			No ports.
		</p>
	{:else}
		<div class="space-y-1.5">
			{#each ports as port (port.id)}
				<div class="rounded-md border border-border/60 bg-muted/20 px-2 py-1.5">
					<div class="flex items-center justify-between">
						<span class="font-mono text-[11px] font-medium text-foreground">{port.id}</span>
						<span class="text-[10px] text-muted-foreground">{port.label}</span>
					</div>
					{#if (port.fields ?? []).length > 0}
						<ul class="mt-1 space-y-0.5">
							{#each port.fields ?? [] as field (field.name)}
								<li class="flex items-center justify-between text-[10px]">
									<span class="font-mono text-foreground">{field.name}</span>
									<span class="text-muted-foreground">
										{field.kind}{field.required ? ' • required' : ''}
									</span>
								</li>
							{/each}
						</ul>
					{:else}
						<p class="mt-1 text-[10px] italic text-muted-foreground">
							Pass-through (no typed fields).
						</p>
					{/if}
				</div>
			{/each}
		</div>
	{/if}
</div>
