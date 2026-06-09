/**
 * Frame-rate-independent joint smoothing for the 3D twins. The joint-state and
 * planning-scene streams deliver poses at the ROS emit rate (typically a few Hz),
 * so applying each frame DIRECTLY makes the arm teleport between poses — at 60fps
 * render the motion reads as a stutter / low framerate even though the GPU is
 * drawing smoothly. This eases every joint from its current angle toward the
 * latest target each animation frame, decoupling visual smoothness from data
 * arrival rate: the arm glides at display rate while data trickles in at 1–5 Hz.
 *
 * The easing is exponential and dt-corrected (`1 - e^(-rate·dt)`), so the
 * convergence speed is identical at 30, 60, or 144 fps — only the step count
 * differs. A grasped sample is parented to a URDF link, so once the joints glide
 * the attached object rides the gripper smoothly with no extra work here.
 *
 * The per-tick math (`alphaFor`, `stepAngles`) is pure and unit-tested; `start`
 * is the thin rAF wrapper that the twins mount while a robot is loaded.
 */
import type { URDFRobot } from 'urdf-loader';

/** Smoothing per second: alpha = 1 − e^(−rate·dt). Frame-rate independent. */
export function alphaFor(rate: number, dt: number): number {
	return 1 - Math.exp(-rate * dt);
}

/**
 * One easing step: mutate `current` toward `target` by `alpha` (0..1). A joint
 * seen for the FIRST time snaps to its target (no sweep from an arbitrary origin
 * — otherwise the arm would lurch from all-zeros on the first frame). A target
 * removed from a later frame keeps its last current value (left untouched).
 */
export function stepAngles(
	current: Map<string, number>,
	target: Map<string, number>,
	alpha: number
): void {
	for (const [name, t] of target) {
		const cur = current.get(name);
		current.set(name, cur === undefined ? t : cur + (t - cur) * alpha);
	}
}

export interface JointInterpolator {
	/** Update the target pose from a freshly-parsed frame (parallel name/value arrays). */
	setTarget(names: string[], positions: number[]): void;
	/** Begin the rAF easing loop against `robot`; returns a stop fn (cancels the loop). */
	start(robot: URDFRobot): () => void;
}

/**
 * Create a joint interpolator. `rate` controls snappiness (higher = tighter
 * tracking, less lag); the default reaches ~target within ~0.4s, which glides
 * cleanly between frames at a 1–5 Hz data rate without visible overshoot.
 */
export function createJointInterpolator(rate = 12): JointInterpolator {
	const target = new Map<string, number>();
	const current = new Map<string, number>();
	return {
		setTarget(names, positions) {
			const n = Math.min(names.length, positions.length);
			for (let i = 0; i < n; i++) {
				const a = positions[i];
				if (typeof a === 'number' && Number.isFinite(a)) target.set(names[i], a);
			}
		},
		start(robot) {
			// Pose the robot at the latest known angles SYNCHRONOUSLY, before the first
			// paint. A freshly (re)built URDFRobot starts at its all-zeros home pose; if
			// we waited for the first rAF tick (~16ms) it would paint one frame at home —
			// the visible "flicker back to home" when the twin remounts (gate/lifecycle
			// churn) while a pose is already known. Snap `current` to any pending target
			// and apply, so the first painted frame is already correct. On a true cold
			// start (no frame yet) target is empty → nothing applied → home is right.
			stepAngles(current, target, 1);
			for (const [name, value] of current) robot.setJointValue(name, value);

			let raf = 0;
			let stopped = false;
			let last = performance.now();
			const tick = (now: number) => {
				if (stopped) return;
				// Clamp the step so a long gap (backgrounded tab pausing rAF) eases in
				// rather than snapping when the tab refocuses.
				const dt = Math.min((now - last) / 1000, 0.1);
				last = now;
				const alpha = alphaFor(rate, dt);
				stepAngles(current, target, alpha);
				for (const [name, value] of current) robot.setJointValue(name, value);
				raf = requestAnimationFrame(tick);
			};
			raf = requestAnimationFrame(tick);
			return () => {
				stopped = true;
				cancelAnimationFrame(raf);
			};
		}
	};
}
