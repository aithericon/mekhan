import type { WithChild } from 'svelte-toolbelt';
import type { ButtonSize, ButtonVariant } from '$lib/components/ui/button';
import type { HTMLAttributes, HTMLButtonAttributes } from 'svelte/elements';
import type { Snippet } from 'svelte';

export type StepperRootProps = {
	step?: number;
	completed?: boolean;
	children?: Snippet;
};

export type StepperNavPropsWithoutHTML = {
	orientation?: 'horizontal' | 'vertical';
};

export type StepperNavProps = StepperNavPropsWithoutHTML & HTMLAttributes<HTMLDivElement>;

export type StepperItemPropsWithoutHTML = {
	id?: string;
};

export type StepperItemProps = StepperItemPropsWithoutHTML &
	Omit<HTMLAttributes<HTMLDivElement>, 'id'>;

export type StepperButtonPropsWithoutHTML = WithChild<{
	disabled?: boolean;
	variant?: ButtonVariant;
	size?: ButtonSize;
}>;

export type StepperButtonProps = StepperButtonPropsWithoutHTML &
	Omit<HTMLButtonAttributes, 'children'>;

export type StepperNextButtonProps = StepperButtonProps;
export type StepperPreviousButtonProps = StepperButtonProps;
