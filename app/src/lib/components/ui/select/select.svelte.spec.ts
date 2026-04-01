import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import SelectTestWrapper from './select-test-wrapper.svelte';

const fruitItems = [
	{ value: 'apple', label: 'Apple' },
	{ value: 'banana', label: 'Banana' },
	{ value: 'cherry', label: 'Cherry' }
];

describe('Select', () => {
	it('renders placeholder text when no value is selected', async () => {
		render(SelectTestWrapper, { placeholder: 'Pick a fruit', items: fruitItems });
		await expect.element(page.getByTestId('select-placeholder')).toHaveTextContent('Pick a fruit');
	});

	it('renders the trigger button', async () => {
		render(SelectTestWrapper, { items: fruitItems });
		await expect.element(page.getByTestId('select-trigger')).toBeInTheDocument();
	});

	it('displays selected value label when value is set', async () => {
		render(SelectTestWrapper, { value: 'banana', items: fruitItems });
		await expect.element(page.getByTestId('select-value')).toHaveTextContent('Banana');
	});

	it('opens content panel when trigger is clicked', async () => {
		render(SelectTestWrapper, { items: fruitItems, open: false });
		await page.getByTestId('select-trigger').click();
		// After clicking, the content should appear with option items
		await expect.element(page.getByText('Apple')).toBeVisible();
		await expect.element(page.getByText('Cherry')).toBeVisible();
	});

	it('renders with no items without errors', async () => {
		render(SelectTestWrapper, { items: [], placeholder: 'No options' });
		await expect.element(page.getByTestId('select-placeholder')).toHaveTextContent('No options');
	});

	it('applies disabled state to select root', async () => {
		const { container } = render(SelectTestWrapper, {
			items: fruitItems,
			disabled: true
		});
		const trigger = container.querySelector('[data-testid="select-trigger"]');
		expect(trigger?.hasAttribute('data-disabled')).toBe(true);
	});
});
