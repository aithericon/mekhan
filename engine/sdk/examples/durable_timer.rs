//! Durable timer examples using the Clockmaster.
//!
//! This example demonstrates two timer patterns:
//!
//! 1. **Fire-and-forget** (`ctx.delay()`) — schedules a durable timer that
//!    fires a signal after a delay. The timer cannot be cancelled once started.
//!
//! 2. **Cancellable timer** (`ctx.timer_with_cancel()`) — schedules a durable
//!    timer that can be cancelled before it fires. Returns `TimerHandles` with
//!    a `scheduled` place (holding `TimerScheduled` tokens with correlation IDs)
//!    and a `cancel_input` place (accepting `TimerCancelInput` tokens to cancel).

use aithericon_sdk::prelude::*;

#[token]
struct Job {
    id: String,
    data: String,
}

#[token]
struct UrgentJob {
    id: String,
    data: String,
    priority: String,
}

fn definition(ctx: &mut Context) {
    // ── Fire-and-forget timer demo ──────────────────────────────────────

    // 1. Places
    let pending = ctx.state::<Job>("pending", "Pending Jobs");
    let ready_to_schedule = ctx.state::<Job>("ready", "Ready to Schedule");
    let completed = ctx.state::<Job>("completed", "Completed");

    // 2. Signal place for timer
    let sig_cooldown_done = ctx.signal::<()>("sig_cooldown_done", "Cooldown Done");

    // 3. Step 1: Start processing
    ctx.transition("start", "Start Processing")
        .auto_input("job", &pending)
        .auto_output("out", &ready_to_schedule)
        .logic(r#"#{ out: job }"#);

    // 4. Step 2: Durable Timer (10 seconds) — single-call helper creates
    //    timer_data place, prep transition, and schedule transition.
    let scheduled = ctx.delay("timer", &ready_to_schedule, 10000, &sig_cooldown_done);

    // 5. Step 3: Finish after timer signal
    ctx.transition("finish", "Finish Job")
        .auto_input("job", &scheduled)
        .auto_input("sig", &sig_cooldown_done)
        .auto_output("out", &completed)
        .logic(r#"#{ out: job + sig }"#);

    // 6. Seed a test job
    ctx.seed_one(
        &pending,
        Job {
            id: "job-1".to_string(),
            data: "some work".to_string(),
        },
    );

    // ── Cancellable timer demo ──────────────────────────────────────────

    let urgent = ctx.state::<UrgentJob>("urgent", "Urgent Jobs");
    let urgent_waiting = ctx.state::<UrgentJob>("urgent_waiting", "Urgent Waiting");
    let sig_urgent_done = ctx.signal::<DynamicToken>("sig_urgent_done", "Urgent Timer Fired");
    let cancel_trigger = ctx.signal::<DynamicToken>("cancel_trigger", "Cancel Trigger");
    let effect_errors = ctx.state::<EffectError>("effect_errors", "Effect Errors");
    let urgent_completed = ctx.state::<UrgentJob>("urgent_completed", "Urgent Completed");
    let urgent_cancelled = ctx.state::<UrgentJob>("urgent_cancelled", "Urgent Cancelled");

    // Start urgent processing
    ctx.transition("start_urgent", "Start Urgent Processing")
        .auto_input("job", &urgent)
        .auto_output("out", &urgent_waiting)
        .logic(r#"#{ out: job }"#);

    // Cancellable timer (30 s) — creates timer_data, prep, schedule, and
    // cancel transitions internally. Returns handles to the scheduled place
    // (TimerScheduled tokens) and the cancel_input place (TimerCancelInput).
    let timer = ctx.timer_with_cancel(
        "urgent_timer",
        &urgent_waiting,
        30000,
        &sig_urgent_done,
        &effect_errors,
    );

    // Normal path: timer fires → mark job as completed.
    // `timer.scheduled` holds TimerScheduled tokens whose `payload` field
    // contains the original UrgentJob data serialised as JSON.
    ctx.transition("urgent_done", "Urgent Timer Done")
        .auto_input("job", &timer.scheduled)
        .auto_input("sig", &sig_urgent_done)
        .auto_output("out", &urgent_completed)
        .logic(
            r#"#{ out: #{ id: job.payload.id, data: job.payload.data, priority: "processed" } }"#,
        );

    // Cancel path: an external cancel signal arrives before the timer fires.
    // We consume the scheduled token and produce a TimerCancelInput so the
    // engine's timer_cancel effect handler tears down the pending timer.
    ctx.transition("cancel_urgent", "Cancel Urgent Timer")
        .auto_input("cancel", &cancel_trigger)
        .auto_input("job", &timer.scheduled)
        .auto_output("cancelled", &urgent_cancelled)
        .auto_output("cancel_input", &timer.cancel_input)
        .logic(r#"#{
            cancelled: #{ id: job.payload.id, data: job.payload.data, priority: "cancelled" },
            cancel_input: #{ timer_correlation_id: job.timer_correlation_id, target_place_id: job.target_place_id }
        }"#);

    // Seed an urgent job
    ctx.seed_one(
        &urgent,
        UrgentJob {
            id: "urgent-1".to_string(),
            data: "time-sensitive work".to_string(),
            priority: "high".to_string(),
        },
    );
}

fn main() {
    aithericon_sdk::run("durable-timer", "Demo of durable delays", definition);
}
