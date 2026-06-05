<script lang="ts">
	// Full-screen "task submitted" confirmation, ported from the HPI app.
	// Fades in over a blurred, emerald-tinted backdrop, pops the card via a
	// scale transition, then draws an animated checkmark (SVG stroke-dashoffset).
	// Auto-dismisses after ~2.2s; click / Escape / Enter / Space also dismiss.
	//
	// The checkmark animation is inlined here (no extra dependency): a static
	// circle plus a check <path> whose stroke is drawn from full offset → 0,
	// held invisible for the first third so the pop registers first.
	import { fade, scale } from 'svelte/transition';

	let {
		visible = false,
		onDismiss,
		title = 'Task Submitted',
		message = 'Response sent successfully'
	}: {
		visible?: boolean;
		onDismiss: () => void;
		title?: string;
		message?: string;
	} = $props();

	let animateIcon = $state(false);

	$effect(() => {
		if (!visible) return;
		const iconTimer = setTimeout(() => {
			animateIcon = true;
		}, 200);
		const dismissTimer = setTimeout(() => {
			onDismiss();
		}, 2200);
		return () => {
			clearTimeout(iconTimer);
			clearTimeout(dismissTimer);
			animateIcon = false;
		};
	});

	function handleKeydown(event: KeyboardEvent): void {
		if (event.key === 'Escape' || event.key === 'Enter' || event.key === ' ') {
			onDismiss();
		}
	}
</script>

{#if visible}
	<div
		class="fixed inset-0 z-[100] flex flex-col items-center justify-center"
		role="button"
		tabindex="-1"
		transition:fade={{ duration: 300 }}
		onclick={onDismiss}
		onkeydown={handleKeydown}
	>
		<div class="absolute inset-0 bg-background/90 backdrop-blur-sm"></div>
		<div
			class="absolute inset-0 bg-[radial-gradient(circle_at_center,_rgba(16,185,129,0.15),_transparent_60%)]"
		></div>

		<div
			class="relative flex flex-col items-center gap-5"
			in:scale={{ duration: 400, start: 0.8, delay: 100 }}
		>
			<div class="text-emerald-500">
				<svg
					xmlns="http://www.w3.org/2000/svg"
					width="80"
					height="80"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="1.5"
					stroke-linecap="round"
					stroke-linejoin="round"
					class="circle-check-icon"
					class:animate={animateIcon}
					aria-label="success"
					role="img"
				>
					<circle cx="12" cy="12" r="10" />
					<path d="m9 12 2 2 4-4" class="check-path" />
				</svg>
			</div>

			<div class="text-center">
				<h2 class="text-2xl font-semibold tracking-tight text-foreground">{title}</h2>
				<p class="mt-1.5 text-sm text-muted-foreground">{message}</p>
			</div>
		</div>
	</div>
{/if}

<style>
	.circle-check-icon {
		overflow: visible;
	}
	.check-path {
		stroke-dasharray: 9;
		stroke-dashoffset: 0;
		transition:
			stroke-dashoffset 0.125s ease-out,
			opacity 0.125s ease-out;
	}
	.circle-check-icon.animate .check-path {
		animation: checkAnimation 0.5s ease-out backwards;
	}
	@keyframes checkAnimation {
		0% {
			stroke-dashoffset: 9;
			opacity: 0;
		}
		33% {
			stroke-dashoffset: 9;
			opacity: 0;
		}
		100% {
			stroke-dashoffset: 0;
			opacity: 1;
		}
	}
</style>
