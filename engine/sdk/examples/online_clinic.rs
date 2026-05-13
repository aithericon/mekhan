//! Online Clinic Workflow - Async Patient Care Lifecycle
//!
//! Models a real-world online mental health clinic with:
//! - Patient intake and document verification
//! - Initial consultation scheduling with doctors
//! - 8-weekly routine checkups (cyclic workflow)
//! - No-show handling with bounded rescheduling
//! - Doctor letter generation and sending
//! - SLA timeout handling (capacity simulation)
//!
//! Demonstrates:
//! - Multi-resource coordination (Doctors, Admin Workers)
//! - Cyclic workflows (checkup → next checkup)
//! - Parallel workflows (letter generation while scheduling next)
//! - Bounded retry patterns (no-show rescheduling)
//! - Two-phase commit (schedule → confirm)
//! - Capacity analysis via SLA timeouts
//!
//! Run with: `cargo run --example online_clinic`
//! Deploy to engine: `cargo run --example online_clinic -- --deploy`
//!
//! Capacity simulation:
//!   `cargo run --example online_clinic -- --deploy --patients 10`
//!   - Low patient count (3-5): All patients get timely appointments
//!   - Medium patient count (10-15): Some SLA timeouts start appearing
//!   - High patient count (30+): Many patients timeout waiting for doctors

use aithericon_sdk::prelude::*;

// ============================================================================
// Token Types - Core Entities
// ============================================================================

/// New patient starting the intake process
#[token]
struct Patient {
    id: String,
    name: String,
    email: String,
}

/// Patient with documents awaiting verification
#[token]
struct IntakeRequest {
    patient_id: String,
    patient_name: String,
    patient_email: String,
    admin_id: String,
    verification_attempts: i64,
    max_verification_attempts: i64,
}

/// Patient approved and ready for care
#[token]
struct ActivePatient {
    id: String,
    name: String,
    email: String,
    assigned_doctor_id: String,
    checkup_count: i64,
}

/// Patient waiting for next checkup to become due (8-week waiting period)
#[token]
struct WaitingForCheckup {
    patient_id: String,
    patient_name: String,
    patient_email: String,
    assigned_doctor_id: String,
    checkup_count: i64,
}

/// Scheduled appointment awaiting patient arrival
#[token]
struct ScheduledAppointment {
    patient_id: String,
    patient_name: String,
    patient_email: String,
    doctor_id: String,
    appointment_type: String, // "initial" or "checkup"
    no_show_count: i64,
    max_no_shows: i64,
}

/// Consultation in progress (doctor + patient meeting)
#[token]
struct InProgressConsultation {
    patient_id: String,
    patient_name: String,
    patient_email: String,
    doctor_id: String,
    appointment_type: String,
    checkup_count: i64,
}

/// Doctor letter awaiting review
#[token]
struct PendingLetter {
    patient_id: String,
    patient_name: String,
    doctor_id: String,
    admin_id: String,
    content: String,
}

/// Completed patient record (discharged or dropped)
#[token]
struct CompletedPatient {
    id: String,
    name: String,
    reason: String,
    total_checkups: i64,
}

/// Sent doctor letter
#[token]
struct SentLetter {
    patient_id: String,
    patient_name: String,
    content: String,
}

// ============================================================================
// Token Types - Resources
// ============================================================================

#[token]
struct Doctor {
    id: String,
    name: String,
    specialty: String,
}

#[token]
struct AdminWorker {
    id: String,
    name: String,
}

// ============================================================================
// Token Types - Signals (Async Events)
// ============================================================================

#[token]
struct DocumentsVerifiedSignal {
    patient_id: String,
    approved: bool,
    reason: String,
}

#[token]
struct AppointmentConfirmedSignal {
    patient_id: String,
}

#[token]
struct PatientCheckedInSignal {
    patient_id: String,
}

#[token]
struct PatientNoShowSignal {
    patient_id: String,
}

#[token]
struct ConsultationCompleteSignal {
    patient_id: String,
    notes: String,
    needs_letter: bool,
    continue_care: bool,
}

#[token]
struct LetterApprovedSignal {
    patient_id: String,
}

#[token]
struct LetterRejectedSignal {
    patient_id: String,
    reason: String,
}

/// SLA timeout signal (patient waited too long for appointment)
#[token]
struct SLATimeoutSignal {
    patient_id: String,
    waited_ms: i64,
    sla_ms: i64,
}

/// Checkup due signal (waiting period elapsed, ready to schedule next checkup)
#[token]
struct CheckupDueSignal {
    patient_id: String,
}

// ============================================================================
// Step Definitions - Intake Phase
// ============================================================================

/// Start intake: Patient + Admin → IntakeRequest
#[step("t_start_intake", "1. Start Intake")]
fn start_intake(patient: Patient, admin: AdminWorker) -> IntakeRequest {
    IntakeRequest {
        patient_id: patient.id,
        patient_name: patient.name,
        patient_email: patient.email,
        admin_id: admin.id,
        verification_attempts: 0,
        max_verification_attempts: 2,
    }
}

/// Intake approved: IntakeRequest + VerifiedSignal(approved=true) → ActivePatient + Admin
/// Note: Doctor assignment is just a reference (round-robin by patient ID), not consumption
#[step("t_intake_approved", "2a. Intake Approved")]
#[guard("req.patient_id == sig.patient_id && sig.approved")]
fn intake_approved(
    req: IntakeRequest,
    sig: DocumentsVerifiedSignal,
    active: Target<ActivePatient>,
    admin: Target<AdminWorker>,
) {
    // Doctor assignment is a reference, not resource consumption
    // Assign all to D001 for simplicity (production would use load balancing)
    r#"
        let doctor = if req.patient_id.len() % 2 == 0 { "D001" } else { "D002" };
        #{
            active: #{
                id: req.patient_id,
                name: req.patient_name,
                email: req.patient_email,
                assigned_doctor_id: doctor,
                checkup_count: 0
            },
            admin: #{ id: req.admin_id, name: "Admin" }
        }
    "#
}

/// Intake rejected (retryable): IntakeRequest + VerifiedSignal(approved=false, retries left) → IntakeRequest
/// Note: Admin stays assigned to this case until intake completes (approved or failed)
#[step("t_intake_retry", "2b. Request More Docs")]
#[guard("req.patient_id == sig.patient_id && !sig.approved && req.verification_attempts < req.max_verification_attempts")]
fn intake_retry(req: IntakeRequest, sig: DocumentsVerifiedSignal, retry: Target<IntakeRequest>) {
    r#"#{
        retry: #{
            patient_id: req.patient_id,
            patient_name: req.patient_name,
            patient_email: req.patient_email,
            admin_id: req.admin_id,
            verification_attempts: req.verification_attempts + 1,
            max_verification_attempts: req.max_verification_attempts
        }
    }"#
}

/// Intake failed (exhausted): IntakeRequest + VerifiedSignal(approved=false, no retries) → CompletedPatient + Admin
#[step("t_intake_failed", "2c. Intake Failed")]
#[guard("req.patient_id == sig.patient_id && !sig.approved && req.verification_attempts >= req.max_verification_attempts")]
fn intake_failed(
    req: IntakeRequest,
    sig: DocumentsVerifiedSignal,
    completed: Target<CompletedPatient>,
    admin: Target<AdminWorker>,
) {
    r#"#{
        completed: #{
            id: req.patient_id,
            name: req.patient_name,
            reason: "Intake failed: " + sig.reason,
            total_checkups: 0
        },
        admin: #{ id: req.admin_id, name: "Admin" }
    }"#
}

// ============================================================================
// Step Definitions - Appointment Scheduling
// ============================================================================

/// Schedule initial consultation: ActivePatient(checkup_count=0) → ScheduledAppointment
/// Doctor is NOT consumed here - only assigned by reference (doctor will be claimed at arrival)
/// Uses Target to allow merging all scheduled appointments into single pending_confirmation place
#[step("t_schedule_initial", "3a. Schedule Initial")]
#[guard("patient.checkup_count == 0")]
fn schedule_initial(patient: ActivePatient, pending: Target<ScheduledAppointment>) {
    r#"#{
        pending: #{
            patient_id: patient.id,
            patient_name: patient.name,
            patient_email: patient.email,
            doctor_id: patient.assigned_doctor_id,
            appointment_type: "initial",
            no_show_count: 0,
            max_no_shows: 3
        }
    }"#
}

/// Schedule routine checkup: ActivePatient(checkup_count>0) → ScheduledAppointment
/// Doctor is NOT consumed here - only assigned by reference (doctor will be claimed at arrival)
/// Uses Target to allow merging all scheduled appointments into single pending_confirmation place
#[step("t_schedule_checkup", "3b. Schedule Checkup")]
#[guard("patient.checkup_count > 0")]
fn schedule_checkup(patient: ActivePatient, pending: Target<ScheduledAppointment>) {
    r#"#{
        pending: #{
            patient_id: patient.id,
            patient_name: patient.name,
            patient_email: patient.email,
            doctor_id: patient.assigned_doctor_id,
            appointment_type: "checkup",
            no_show_count: 0,
            max_no_shows: 3
        }
    }"#
}

/// Confirm appointment: ScheduledAppointment + ConfirmedSignal → ScheduledAppointment (awaiting day)
/// Uses Target to allow multiple scheduling sources to merge into shared confirmed place
#[step("t_confirm_appointment", "4. Confirm Appointment")]
#[guard("appt.patient_id == sig.patient_id")]
fn confirm_appointment(
    appt: ScheduledAppointment,
    sig: AppointmentConfirmedSignal,
    confirmed: Target<ScheduledAppointment>,
) {
    r#"#{
        confirmed: #{
            patient_id: appt.patient_id,
            patient_name: appt.patient_name,
            patient_email: appt.patient_email,
            doctor_id: appt.doctor_id,
            appointment_type: appt.appointment_type,
            no_show_count: appt.no_show_count,
            max_no_shows: appt.max_no_shows
        }
    }"#
}

// ============================================================================
// Step Definitions - Patient Arrival (Show/No-Show)
// ============================================================================

/// Patient shows up: ScheduledAppointment + CheckedInSignal + Doctor → InProgressConsultation
/// Doctor is consumed HERE at consultation start - doctor becomes busy
/// Uses Target to merge all arrival paths into single consultation place
#[step("t_patient_arrived", "5a. Patient Arrived")]
#[guard("appt.patient_id == sig.patient_id && appt.doctor_id == doctor.id")]
fn patient_arrived(
    appt: ScheduledAppointment,
    sig: PatientCheckedInSignal,
    doctor: Doctor,
    consultation: Target<InProgressConsultation>,
) {
    r#"#{
        consultation: #{
            patient_id: appt.patient_id,
            patient_name: appt.patient_name,
            patient_email: appt.patient_email,
            doctor_id: doctor.id,
            appointment_type: appt.appointment_type,
            checkup_count: 0
        }
    }"#
}

/// Patient no-show (retryable): ScheduledAppointment + NoShowSignal → Reschedule
/// Doctor is NOT returned here - they were never consumed (only consumed at patient_arrived)
/// Uses Target to allow wiring output back to scheduling queue
#[step("t_no_show_reschedule", "5b. No-Show (Reschedule)")]
#[guard("appt.patient_id == sig.patient_id && appt.no_show_count < appt.max_no_shows")]
fn no_show_reschedule(
    appt: ScheduledAppointment,
    sig: PatientNoShowSignal,
    reschedule: Target<ScheduledAppointment>,
) {
    r#"#{
        reschedule: #{
            patient_id: appt.patient_id,
            patient_name: appt.patient_name,
            patient_email: appt.patient_email,
            doctor_id: appt.doctor_id,
            appointment_type: appt.appointment_type,
            no_show_count: appt.no_show_count + 1,
            max_no_shows: appt.max_no_shows
        }
    }"#
}

/// Patient no-show (exhausted): ScheduledAppointment + NoShowSignal → Dropped
/// Doctor is NOT returned here - they were never consumed (only consumed at patient_arrived)
/// Uses Target to wire to shared completed place
#[step("t_no_show_dropped", "5c. No-Show (Dropped)")]
#[guard("appt.patient_id == sig.patient_id && appt.no_show_count >= appt.max_no_shows")]
fn no_show_dropped(
    appt: ScheduledAppointment,
    sig: PatientNoShowSignal,
    dropped: Target<CompletedPatient>,
) {
    r#"#{
        dropped: #{
            id: appt.patient_id,
            name: appt.patient_name,
            reason: "Dropped: Too many no-shows",
            total_checkups: 0
        }
    }"#
}

/// SLA timeout (with check-in): ScheduledAppointment + SLATimeoutSignal + CheckedInSignal → Dropped
/// Patient checked in but waited too long for a doctor and is dropped
/// This transition has priority over sla_timeout because it has more inputs
#[step("t_sla_timeout_checkedin", "5d. SLA Timeout (Checked In)")]
#[guard("appt.patient_id == timeout_sig.patient_id && appt.patient_id == checkin_sig.patient_id")]
fn sla_timeout_checkedin(
    appt: ScheduledAppointment,
    timeout_sig: SLATimeoutSignal,
    checkin_sig: CheckedInSignal,
    dropped: Target<CompletedPatient>,
) {
    r#"#{
        dropped: #{
            id: appt.patient_id,
            name: appt.patient_name,
            reason: "SLA timeout (checked in): waited " + timeout_sig.waited_ms + "ms (max: " + timeout_sig.sla_ms + "ms)",
            total_checkups: 0
        }
    }"#
}

/// SLA timeout (not checked in): ScheduledAppointment + SLATimeoutSignal → Dropped
/// Patient timed out before even checking in
#[step("t_sla_timeout", "5e. SLA Timeout (Not Checked In)")]
#[guard("appt.patient_id == sig.patient_id")]
fn sla_timeout(
    appt: ScheduledAppointment,
    sig: SLATimeoutSignal,
    dropped: Target<CompletedPatient>,
) {
    r#"#{
        dropped: #{
            id: appt.patient_id,
            name: appt.patient_name,
            reason: "SLA timeout: waited " + sig.waited_ms + "ms (max: " + sig.sla_ms + "ms)",
            total_checkups: 0
        }
    }"#
}

// ============================================================================
// Step Definitions - Consultation Completion
// ============================================================================

/// Consultation complete (continue care): → WaitingForCheckup + Doctor
/// Patient enters waiting period before next checkup can be scheduled
#[step("t_consultation_complete_continue", "6a. Continue Care")]
#[guard("consult.patient_id == sig.patient_id && sig.continue_care && !sig.needs_letter")]
fn consultation_complete_continue(
    consult: InProgressConsultation,
    sig: ConsultationCompleteSignal,
    waiting: Target<WaitingForCheckup>,
    doctor: Target<Doctor>,
) {
    r#"#{
        waiting: #{
            patient_id: consult.patient_id,
            patient_name: consult.patient_name,
            patient_email: consult.patient_email,
            assigned_doctor_id: consult.doctor_id,
            checkup_count: consult.checkup_count + 1
        },
        doctor: #{ id: consult.doctor_id, name: "Doctor", specialty: "General" }
    }"#
}

/// Consultation complete (needs letter + continue): → WaitingForCheckup + PendingLetter + Doctor
/// Patient enters waiting period before next checkup, letter workflow runs in parallel
#[step("t_consultation_with_letter", "6b. Generate Letter + Continue")]
#[guard("consult.patient_id == sig.patient_id && sig.continue_care && sig.needs_letter")]
fn consultation_with_letter(
    consult: InProgressConsultation,
    sig: ConsultationCompleteSignal,
    admin: AdminWorker,
    waiting: Target<WaitingForCheckup>,
    letter: Target<PendingLetter>,
    doctor: Target<Doctor>,
) {
    r#"#{
        waiting: #{
            patient_id: consult.patient_id,
            patient_name: consult.patient_name,
            patient_email: consult.patient_email,
            assigned_doctor_id: consult.doctor_id,
            checkup_count: consult.checkup_count + 1
        },
        letter: #{
            patient_id: consult.patient_id,
            patient_name: consult.patient_name,
            doctor_id: consult.doctor_id,
            admin_id: admin.id,
            content: sig.notes
        },
        doctor: #{ id: consult.doctor_id, name: "Doctor", specialty: "General" }
    }"#
}

/// Consultation complete (discharge): → CompletedPatient + Doctor
#[step("t_consultation_discharge", "6c. Discharge Patient")]
#[guard("consult.patient_id == sig.patient_id && !sig.continue_care")]
fn consultation_discharge(
    consult: InProgressConsultation,
    sig: ConsultationCompleteSignal,
    completed: Target<CompletedPatient>,
    doctor: Target<Doctor>,
) {
    r#"#{
        completed: #{
            id: consult.patient_id,
            name: consult.patient_name,
            reason: "Discharged: Treatment complete",
            total_checkups: consult.checkup_count + 1
        },
        doctor: #{ id: consult.doctor_id, name: "Doctor", specialty: "General" }
    }"#
}

// ============================================================================
// Step Definitions - Letter Workflow
// ============================================================================

/// Letter approved: PendingLetter + ApprovedSignal → SentLetter + Admin
#[step("t_letter_approved", "7a. Letter Approved")]
#[guard("letter.patient_id == sig.patient_id")]
fn letter_approved(
    letter: PendingLetter,
    sig: LetterApprovedSignal,
    sent: Target<SentLetter>,
    admin: Target<AdminWorker>,
) {
    r#"#{
        sent: #{
            patient_id: letter.patient_id,
            patient_name: letter.patient_name,
            content: letter.content
        },
        admin: #{ id: letter.admin_id, name: "Admin" }
    }"#
}

/// Letter rejected (needs revision): PendingLetter + RejectedSignal → PendingLetter
/// Note: Admin stays assigned to this letter until it's approved
#[step("t_letter_rejected", "7b. Letter Rejected")]
#[guard("letter.patient_id == sig.patient_id")]
fn letter_rejected(
    letter: PendingLetter,
    sig: LetterRejectedSignal,
    revised: Target<PendingLetter>,
) {
    r#"#{
        revised: #{
            patient_id: letter.patient_id,
            patient_name: letter.patient_name,
            doctor_id: letter.doctor_id,
            admin_id: letter.admin_id,
            content: letter.content + " [REVISED]"
        }
    }"#
}

// ============================================================================
// Step Definitions - Checkup Scheduling (Waiting Period)
// ============================================================================

/// Checkup becomes due: WaitingForCheckup + CheckupDueSignal → ActivePatient
/// After the waiting period, patient re-enters the active pool for scheduling
#[step("t_checkup_ready", "8. Checkup Ready")]
#[guard("waiting.patient_id == sig.patient_id")]
fn checkup_ready(
    waiting: WaitingForCheckup,
    sig: CheckupDueSignal,
    patient: Target<ActivePatient>,
) {
    r#"#{
        patient: #{
            id: waiting.patient_id,
            name: waiting.patient_name,
            email: waiting.patient_email,
            assigned_doctor_id: waiting.assigned_doctor_id,
            checkup_count: waiting.checkup_count
        }
    }"#
}

// ============================================================================
// Workflow Definition
// ============================================================================

/// Get patient count from CLI argument `--patients N` (default: 3)
fn get_patient_count() -> usize {
    std::env::args()
        .position(|a| a == "--patients")
        .and_then(|i| std::env::args().nth(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(3)
}

/// Generate N patients with sequential IDs and names
fn generate_patients(count: usize) -> Vec<Patient> {
    const FIRST_NAMES: &[&str] = &[
        "Alice", "Bob", "Carol", "David", "Emma", "Frank", "Grace", "Henry", "Ivy", "Jack", "Kate",
        "Leo", "Mia", "Noah", "Olivia", "Peter", "Quinn", "Rose", "Sam", "Tina", "Uma", "Victor",
        "Wendy", "Xavier", "Yara", "Zack",
    ];
    const LAST_NAMES: &[&str] = &[
        "Smith",
        "Jones",
        "White",
        "Brown",
        "Davis",
        "Wilson",
        "Moore",
        "Taylor",
        "Anderson",
        "Thomas",
        "Jackson",
        "Harris",
        "Martin",
        "Garcia",
        "Martinez",
        "Robinson",
        "Clark",
        "Rodriguez",
        "Lewis",
        "Lee",
        "Walker",
        "Hall",
        "Allen",
        "Young",
        "King",
        "Wright",
    ];

    (0..count)
        .map(|i| {
            let first = FIRST_NAMES[i % FIRST_NAMES.len()];
            let last = LAST_NAMES[i % LAST_NAMES.len()];
            Patient {
                id: format!("P{:03}", i + 1),
                name: format!("{} {}", first, last),
                email: format!(
                    "{}.{}@example.com",
                    first.to_lowercase(),
                    last.to_lowercase()
                ),
            }
        })
        .collect()
}

fn definition(ctx: &mut Context) {
    // Get patient count from CLI
    let patient_count = get_patient_count();
    eprintln!(
        "Generating {} patients for capacity simulation",
        patient_count
    );

    // === Resource Pools ===
    let patients = ctx.state::<Patient>("p_new_patients", "New Patients");
    let doctors = ctx.state::<Doctor>("p_doctors", "Doctor Pool");
    let admins = ctx.state::<AdminWorker>("p_admins", "Admin Pool");

    // === State Places (shared across workflow branches) ===
    let active_patients = ctx.state::<ActivePatient>("p_active", "Active Patients");
    let letters_pending = ctx.state::<PendingLetter>("p_letters_pending", "Letters Pending");
    let waiting_for_checkup =
        ctx.state::<WaitingForCheckup>("p_waiting_checkup", "Waiting for Checkup");

    // === Signal Places ===
    let sig_docs_verified =
        ctx.signal::<DocumentsVerifiedSignal>("p_sig_docs_verified", "Sig: Docs Verified");
    let sig_appt_confirmed =
        ctx.signal::<AppointmentConfirmedSignal>("p_sig_appt_confirmed", "Sig: Appt Confirmed");
    let sig_checked_in =
        ctx.signal::<PatientCheckedInSignal>("p_sig_checked_in", "Sig: Checked In");
    let sig_no_show = ctx.signal::<PatientNoShowSignal>("p_sig_no_show", "Sig: No Show");
    let sig_consult_complete =
        ctx.signal::<ConsultationCompleteSignal>("p_sig_consult_complete", "Sig: Consult Complete");
    let sig_letter_approved =
        ctx.signal::<LetterApprovedSignal>("p_sig_letter_approved", "Sig: Letter Approved");
    let sig_letter_rejected =
        ctx.signal::<LetterRejectedSignal>("p_sig_letter_rejected", "Sig: Letter Rejected");
    let sig_sla_timeout = ctx.signal::<SLATimeoutSignal>("p_sig_sla_timeout", "Sig: SLA Timeout");
    let sig_checkup_due = ctx.signal::<CheckupDueSignal>("p_sig_checkup_due", "Sig: Checkup Due");

    // === Terminal Places ===
    let completed = ctx.state::<CompletedPatient>("p_completed", "Completed/Dropped");
    let sent_letters = ctx.state::<SentLetter>("p_sent_letters", "Sent Letters");

    // === Seed Initial Data ===
    ctx.seed(&patients, generate_patients(patient_count));

    ctx.seed(
        &doctors,
        vec![Doctor {
            id: "D001".into(),
            name: "Dr. Sarah Chen".into(),
            specialty: "Psychiatry".into(),
        }],
    );

    ctx.seed(
        &admins,
        vec![
            AdminWorker {
                id: "A001".into(),
                name: "Admin Jane".into(),
            },
            AdminWorker {
                id: "A002".into(),
                name: "Admin John".into(),
            },
        ],
    );

    // ========================================================================
    // Wire Up Steps - Intake Phase
    // ========================================================================

    // 1. Start intake: Patient + Admin → IntakeRequest
    let intake_pending = start_intake(ctx, &patients, &admins);

    // 2a. Intake approved: IntakeRequest + Signal → ActivePatient + Admin
    intake_approved(
        ctx,
        &intake_pending,
        &sig_docs_verified,
        &active_patients,
        &admins,
    );

    // 2b. Intake retry: IntakeRequest + Signal → IntakeRequest (admin stays assigned)
    intake_retry(ctx, &intake_pending, &sig_docs_verified, &intake_pending);

    // 2c. Intake failed: IntakeRequest + Signal → Completed + Admin
    intake_failed(
        ctx,
        &intake_pending,
        &sig_docs_verified,
        &completed,
        &admins,
    );

    // ========================================================================
    // Wire Up Steps - Appointment Scheduling
    // ========================================================================

    // Single place for all appointments pending confirmation (initial, checkup, reschedule)
    let pending_confirmation =
        ctx.state::<ScheduledAppointment>("p_pending_confirm", "Pending Confirmation");

    // 3a/3b. Schedule: ActivePatient → single pending confirmation place
    schedule_initial(ctx, &active_patients, &pending_confirmation);
    schedule_checkup(ctx, &active_patients, &pending_confirmation);

    // Shared places for consolidated downstream flow
    let confirmed_appointments =
        ctx.state::<ScheduledAppointment>("p_confirmed", "Confirmed Appointments");
    let in_consultation =
        ctx.state::<InProgressConsultation>("p_in_consultation", "In Consultation");

    // 4. Confirm appointment: single pending → single confirmed (ONE transition)
    confirm_appointment(
        ctx,
        &pending_confirmation,
        &sig_appt_confirmed,
        &confirmed_appointments,
    );

    // ========================================================================
    // Wire Up Steps - Patient Arrival (Single Path)
    // ========================================================================

    // 5a. Patient arrived: Confirmed + CheckedIn + Doctor → single consultation place
    patient_arrived(
        ctx,
        &confirmed_appointments,
        &sig_checked_in,
        &doctors,
        &in_consultation,
    );

    // 5b. No-show reschedule: Confirmed + NoShow → back to pending confirmation
    // Rescheduled appointments go back through the same confirmation flow
    no_show_reschedule(
        ctx,
        &confirmed_appointments,
        &sig_no_show,
        &pending_confirmation,
    );

    // 5c. No-show dropped: Confirmed + NoShow → Completed
    no_show_dropped(ctx, &confirmed_appointments, &sig_no_show, &completed);

    // 5d. SLA timeout (checked in): Confirmed + SLATimeout + CheckedIn → Completed (dropped)
    sla_timeout_checkedin(
        ctx,
        &confirmed_appointments,
        &sig_sla_timeout,
        &sig_checked_in,
        &completed,
    );

    // 5e. SLA timeout (not checked in): Confirmed + SLATimeout → Completed (dropped)
    sla_timeout(ctx, &confirmed_appointments, &sig_sla_timeout, &completed);

    // ========================================================================
    // Wire Up Steps - Consultation Completion (Single Path)
    // ========================================================================

    // 6a. Continue care (no letter): InConsultation + Complete → WaitingForCheckup + Doctor
    consultation_complete_continue(
        ctx,
        &in_consultation,
        &sig_consult_complete,
        &waiting_for_checkup,
        &doctors,
    );

    // 6b. Generate letter + continue: InConsultation + Complete + Admin → WaitingForCheckup + Letter + Doctor
    consultation_with_letter(
        ctx,
        &in_consultation,
        &sig_consult_complete,
        &admins,
        &waiting_for_checkup,
        &letters_pending,
        &doctors,
    );

    // 6c. Discharge: InConsultation + Complete → Completed + Doctor
    consultation_discharge(
        ctx,
        &in_consultation,
        &sig_consult_complete,
        &completed,
        &doctors,
    );

    // ========================================================================
    // Wire Up Steps - Letter Workflow
    // ========================================================================

    // 7a. Letter approved: PendingLetter + Approved → SentLetter + Admin
    letter_approved(
        ctx,
        &letters_pending,
        &sig_letter_approved,
        &sent_letters,
        &admins,
    );

    // 7b. Letter rejected: PendingLetter + Rejected → PendingLetter (admin stays assigned)
    letter_rejected(
        ctx,
        &letters_pending,
        &sig_letter_rejected,
        &letters_pending,
    );

    // ========================================================================
    // Wire Up Steps - Checkup Waiting Period
    // ========================================================================

    // 8. Checkup ready: WaitingForCheckup + CheckupDue → ActivePatient (ready to schedule)
    checkup_ready(
        ctx,
        &waiting_for_checkup,
        &sig_checkup_due,
        &active_patients,
    );

    // ========================================================================
    // Mock Adapters - Simulating External Systems
    // ========================================================================

    // Document Verification System (Admin reviews uploaded docs)
    ctx.mock_adapter(
        &intake_pending,
        "Document Verification",
        1500,
        format!(
            r#"
            let r = random();
            if r < 0.7 {{
                // Approved
                #{{ target_place: "{}", data: #{{ patient_id: token.patient_id, approved: true, reason: "All documents valid" }} }}
            }} else if r < 0.9 {{
                // Rejected but retryable (missing docs)
                #{{ target_place: "{}", data: #{{ patient_id: token.patient_id, approved: false, reason: "Missing insurance card" }} }}
            }} else {{
                // Rejected (fraud or eligibility issue)
                #{{ target_place: "{}", data: #{{ patient_id: token.patient_id, approved: false, reason: "Not eligible for service" }} }}
            }}
            "#,
            sig_docs_verified.id(),
            sig_docs_verified.id(),
            sig_docs_verified.id()
        ),
    );

    // Calendar System - single adapter for ALL pending confirmations (initial, checkup, reschedule)
    ctx.mock_adapter(
        &pending_confirmation,
        "Calendar System",
        800,
        format!(
            r#"
            // Calendar always confirms for this simulation
            #{{ target_place: "{}", data: #{{ patient_id: token.patient_id }} }}
            "#,
            sig_appt_confirmed.id()
        ),
    );

    // Appointment Day (Patient shows or no-shows) - fires quickly when patient arrives
    ctx.mock_adapter(
        &confirmed_appointments,
        "Appointment Day",
        2000,
        format!(
            r#"
            // Normal show/no-show logic
            let r = random();
            // Show rate decreases with no-show count (85% base, 5% penalty per prior no-show)
            let show_rate = 0.85 - (token.no_show_count * 0.05);
            if r < show_rate {{
                // Patient shows up
                #{{ target_place: "{}", data: #{{ patient_id: token.patient_id }} }}
            }} else {{
                // No-show
                #{{ target_place: "{}", data: #{{ patient_id: token.patient_id }} }}
            }}
            "#,
            sig_checked_in.id(),
            sig_no_show.id()
        ),
    );

    // SLA Timeout Adapter - fires after SLA duration, but ONLY if token still exists
    // (meaning patient hasn't shown up or been marked as no-show yet)
    ctx.timeout_adapter(
        &confirmed_appointments,
        "SLA Timeout Monitor",
        10000, // 10 second SLA - fires after this delay
        format!(
            r#"
            // Token still here after SLA => emit timeout signal
            let age_ms = timestamp() - token_created_at;
            #{{ target_place: "{}", data: #{{
                patient_id: token.patient_id,
                waited_ms: age_ms,
                sla_ms: 10000
            }} }}
            "#,
            sig_sla_timeout.id()
        ),
    );

    // Consultation System (Doctor meeting) - single adapter for all consultations
    ctx.mock_adapter(
        &in_consultation,
        "Consultation",
        3000,
        format!(
            r#"
            let needs_letter = random() < 0.3; // 30% need letters
            // Initial consultations always continue; checkups have 90% continue rate
            let continue_care = if token.appointment_type == "initial" {{ true }} else {{ random() < 0.9 }};

            #{{
                target_place: "{}",
                data: #{{
                    patient_id: token.patient_id,
                    notes: token.appointment_type + " notes for " + token.patient_name,
                    needs_letter: needs_letter,
                    continue_care: continue_care
                }}
            }}
            "#,
            sig_consult_complete.id()
        ),
    );

    // Letter Review System
    ctx.mock_adapter(
        &letters_pending,
        "Letter Review",
        1000,
        format!(
            r#"
            let r = random();
            if r < 0.9 {{
                // Approved (90%)
                #{{ target_place: "{}", data: #{{ patient_id: token.patient_id }} }}
            }} else {{
                // Needs revision
                #{{ target_place: "{}", data: #{{ patient_id: token.patient_id, reason: "Needs clarification" }} }}
            }}
            "#,
            sig_letter_approved.id(),
            sig_letter_rejected.id()
        ),
    );

    // Checkup Waiting Period - fires after waiting period to make patient ready for next checkup
    // Uses timeout_adapter because this is an expected delay (like SLA timeout but for normal flow)
    ctx.timeout_adapter(
        &waiting_for_checkup,
        "Checkup Scheduler",
        20000, // 20 second wait (simulates 8-week waiting period)
        format!(
            r#"
            // Waiting period elapsed - patient is now due for checkup
            #{{ target_place: "{}", data: #{{ patient_id: token.patient_id }} }}
            "#,
            sig_checkup_due.id()
        ),
    );
}

fn main() {
    aithericon_sdk::run(
        "online-clinic",
        "Online mental health clinic workflow: intake verification, doctor consultations, 8-weekly checkups, no-show handling, and doctor letter generation.",
        definition,
    );
}
