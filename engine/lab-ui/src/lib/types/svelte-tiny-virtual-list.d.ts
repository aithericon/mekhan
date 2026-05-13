declare module 'svelte-tiny-virtual-list' {
	import type { SvelteComponent, Snippet } from 'svelte';

	interface VirtualListProps {
		width?: string | number;
		height: number;
		itemCount: number;
		itemSize: number | number[] | ((index: number) => number);
		estimatedItemSize?: number;
		overscanCount?: number;
		stickyIndices?: number[];
		getKey?: (index: number) => number | string;
		scrollDirection?: 'vertical' | 'horizontal';
		scrollOffset?: number;
		scrollToIndex?: number;
		scrollToAlignment?: 'start' | 'center' | 'end' | 'auto';
		scrollToBehaviour?: 'smooth' | 'instant' | 'auto';
		recomputeSizes?: (startIndex?: number) => void;
		item?: Snippet<[{ index: number; style: string }]>;
		header?: Snippet;
		footer?: Snippet;
	}

	export default class VirtualList extends SvelteComponent<VirtualListProps> {
		get recomputeSizes(): (startIndex?: number) => void;
	}
}
