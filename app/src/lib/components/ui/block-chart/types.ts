export type ChartType = 'area' | 'bar' | 'line' | 'pie';

export type ChartSeries = {
	key: string;
	label?: string;
	color?: string;
};

export type BlockChartProps = {
	chart_type: ChartType;
	data: Record<string, unknown>[];
	x?: string;
	series?: ChartSeries[];
	caption?: string;
	height?: string;
	x_label?: string;
	y_label?: string;
};
