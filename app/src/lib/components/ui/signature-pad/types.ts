export type SignaturePadProps = {
	/** Current JSON string value (SignatureValue or empty) */
	value?: string;
	/** Callback when signature changes — receives JSON string or empty string */
	onchange?: (value: string) => void;
	/** Pen stroke color. Defaults to '#1e293b' (slate-800) */
	penColor?: string;
	/** Whether the pad is disabled / read-only */
	disabled?: boolean;
	/** HTML id for accessibility */
	id?: string;
	/** data-testid for testing */
	'data-testid'?: string;
	/** ARIA attributes forwarded from form control */
	'aria-invalid'?: string;
	'aria-describedby'?: string;
};
