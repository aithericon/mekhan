<script lang="ts">
	// Template detail layout — a thin tab strip (Editor / Analytics) sits above
	// the independently-scrolling / bleed child routes:
	//
	//   /templates/[id]           — TemplateEditor (canonical full-bleed canvas)
	//   /templates/[id]/analytics — scrollable analytics page (no own PageShell)
	//
	// The bare route is the canonical editor everything navigates to (list, home,
	// folders, version switch), so the "Editor" tab targets it — exact-matched so
	// it doesn't also light up under /analytics (a prefix of the bare path). The
	// secondary IDE surface (/templates/[id]/ide, IdeWorkbench) opts OUT of this
	// layout via its own `+page@.svelte` reset: it's a deliberately-entered
	// full-screen workbench with its own toolbar + back button and would
	// otherwise get a redundant second chrome strip.
	//
	// Using `width="bleed"` (just a `h-full` div) so the editor canvas can expand
	// to fill the remaining height after the tab strip. The analytics page adds
	// its own `overflow-y-auto` wrapper for scrollability.
	import { page } from '$app/state';
	import { PageShell, PageTabs, type PageTab } from '$lib/components/shell';
	import BarChart2 from '@lucide/svelte/icons/bar-chart-2';
	import PenLine from '@lucide/svelte/icons/pen-line';

	let { children } = $props();

	const id = $derived(page.params.id!);

	const templateTabs = $derived<PageTab[]>([
		{
			href: `/templates/${id}`,
			exact: true,
			label: 'Editor',
			icon: PenLine,
			title: 'Visual workflow editor',
			testid: 'template-tab-editor'
		},
		{
			href: `/templates/${id}/analytics`,
			label: 'Analytics',
			icon: BarChart2,
			title: 'Run usage, duration percentiles, and node hotspots',
			testid: 'template-tab-analytics'
		}
	]);
</script>

<PageShell width="bleed" testid="template-detail-page">
	<div class="flex h-full flex-col">
		<!-- Minimal tab strip — no full PageHeader so editor canvases don't lose
		     vertical space to an unnecessary title row. The `pt-1` and `-mb-px`
		     pull the 2-px active underline flush onto the border-b (same trick as
		     the band variant's `tabs` snippet wrapper). -->
		<div class="shrink-0 border-b border-border bg-card px-6 pt-1">
			<div class="-mb-px">
				<PageTabs tabs={templateTabs} testid="template-tabs" />
			</div>
		</div>

		<!-- Children render here. Editor routes supply their own full-bleed
		     PageShell (a `h-full` div) which correctly fills this flex-1 slot.
		     The analytics page adds its own overflow-y-auto scroller. -->
		<div class="relative min-h-0 flex-1">
			{@render children()}
		</div>
	</div>
</PageShell>
