<script lang="ts">
	// Nested-corners brand motif (companion to BrandSpiral). The original art
	// is white→transparent gradients on a dark plate; we drop the plate and
	// drive the stops from `currentColor` so it themes with light/dark
	// automatically (set the colour + opacity via the `class` prop).
	let { class: className = '' }: { class?: string } = $props();
</script>

<svg
	class={className}
	xmlns="http://www.w3.org/2000/svg"
	viewBox="0 0 100 100"
	fill="none"
	aria-hidden="true"
	focusable="false"
>
	<path d="M22 50V78H50M50 22H78V50" stroke="url(#bc-outer)" stroke-width="8" />
	<path d="M30 50L30 30H50M50 70H70V50" stroke="url(#bc-inner)" stroke-width="8" />
	<rect x="34" y="66" width="16" height="16" transform="rotate(-90 34 66)" fill="url(#bc-sq-a)" />
	<rect x="50" y="50.0001" width="16" height="16" transform="rotate(-90 50 50.0001)" fill="url(#bc-sq-b)" />
	<defs>
		<linearGradient id="bc-outer" x1="20" y1="50" x2="80" y2="50" gradientUnits="userSpaceOnUse">
			<stop stop-color="currentColor" />
			<stop offset="1" stop-color="currentColor" stop-opacity="0.26" />
		</linearGradient>
		<linearGradient
			id="bc-inner"
			x1="29.8995"
			y1="50.1006"
			x2="69.8995"
			y2="50.1006"
			gradientUnits="userSpaceOnUse"
		>
			<stop stop-color="currentColor" />
			<stop offset="1" stop-color="currentColor" stop-opacity="0.26" />
		</linearGradient>
		<linearGradient
			id="bc-sq-a"
			x1="41.4667"
			y1="67.3333"
			x2="40.5333"
			y2="83.8667"
			gradientUnits="userSpaceOnUse"
		>
			<stop stop-color="currentColor" />
			<stop offset="1" stop-color="currentColor" stop-opacity="0" />
		</linearGradient>
		<linearGradient
			id="bc-sq-b"
			x1="57.4667"
			y1="51.3334"
			x2="56.5333"
			y2="67.8667"
			gradientUnits="userSpaceOnUse"
		>
			<stop stop-color="currentColor" />
			<stop offset="1" stop-color="currentColor" stop-opacity="0" />
		</linearGradient>
	</defs>
</svg>
