<script lang="ts">
	// SPDX-License-Identifier: Apache-2.0
	import ArrowDown from '@lucide/svelte/icons/arrow-down';
	import ArrowUp from '@lucide/svelte/icons/arrow-up';
	import ChevronsUpDown from '@lucide/svelte/icons/chevrons-up-down';
	import { type SortingState, getCoreRowModel, getSortedRowModel } from '@tanstack/table-core';
	import { createSvelteTable } from '$lib/components/ui/data-table';
	import { tableColumns } from '../utils';

	let { headers, rows, alignments, caption, renderCell, sortable = true }: {
		headers: string[];
		rows: string[][];
		alignments?: ('left' | 'center' | 'right')[];
		caption?: string;
		renderCell?: (text: string) => string;
		sortable?: boolean;
	} = $props();

	function cellHtml(text: string): string {
		return renderCell ? renderCell(text) : text;
	}

	const columns = $derived(tableColumns(headers));

	let sorting = $state<SortingState>([]);

	const table = createSvelteTable({
		get data() {
			return rows;
		},
		get columns() {
			return columns;
		},
		get enableSorting() {
			return sortable;
		},
		state: {
			get sorting() {
				return sorting;
			}
		},
		onSortingChange: (updater) => {
			sorting = typeof updater === 'function' ? updater(sorting) : updater;
		},
		getCoreRowModel: getCoreRowModel(),
		getSortedRowModel: getSortedRowModel()
	});

	const JUSTIFY = { left: 'flex-start', center: 'center', right: 'flex-end' } as const;

	function ariaSort(i: number): 'ascending' | 'descending' | undefined {
		const dir = table.getColumn(String(i))?.getIsSorted();
		return dir === 'asc' ? 'ascending' : dir === 'desc' ? 'descending' : undefined;
	}
</script>

<div class="overflow-hidden rounded-xl border border-border bg-card/70 shadow-sm">
	{#if caption}
		<p class="pt-3 pb-2 text-center text-base text-muted-foreground">{caption}</p>
	{/if}
	<div class="overflow-x-auto">
		<table class="w-full border-collapse">
			<thead>
				<tr class="border-t border-b border-border">
					{#each headers as header, i (i)}
						{@const column = table.getColumn(String(i))}
						<th
							class="border-r border-border px-4 py-2 text-left text-base font-semibold text-foreground last:border-r-0"
							style="text-align: {alignments?.[i] ?? 'left'}"
							aria-sort={ariaSort(i)}
						>
							{#if sortable && column}
								<button
									type="button"
									class="flex w-full cursor-pointer items-center gap-1.5 select-none hover:text-muted-foreground"
									style="justify-content: {JUSTIFY[alignments?.[i] ?? 'left']}"
									onclick={column.getToggleSortingHandler()}
								>
									<!-- eslint-disable-next-line svelte/no-at-html-tags -->
									<span>{@html cellHtml(header)}</span>
									{#if column.getIsSorted() === 'asc'}
										<ArrowUp class="size-3.5 shrink-0" />
									{:else if column.getIsSorted() === 'desc'}
										<ArrowDown class="size-3.5 shrink-0" />
									{:else}
										<ChevronsUpDown class="size-3.5 shrink-0 opacity-40" />
									{/if}
								</button>
							{:else}
								<!-- eslint-disable-next-line svelte/no-at-html-tags -->
								{@html cellHtml(header)}
							{/if}
						</th>
					{/each}
				</tr>
			</thead>
			<tbody>
				{#each table.getRowModel().rows as tableRow, rowIdx (tableRow.id)}
					<tr class="{rowIdx % 2 === 1 ? 'bg-muted/20' : ''} border-b border-border last:border-b-0">
						{#each tableRow.original as cell, i (i)}
							<td class="border-r border-border px-4 py-2 text-base text-foreground last:border-r-0"
								style="text-align: {alignments?.[i] ?? 'left'}">
								<!-- eslint-disable-next-line svelte/no-at-html-tags -->
								{@html cellHtml(cell)}
							</td>
						{/each}
					</tr>
				{/each}
			</tbody>
		</table>
	</div>
</div>
