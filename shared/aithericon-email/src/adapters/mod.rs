//! Concrete [`Mailer`](crate::Mailer) implementations. All are compiled in;
//! [`crate::build_mailer`] picks one at runtime from [`MailerConfig`](crate::MailerConfig).

pub mod brevo;
pub mod log;
pub mod smtp;

pub use brevo::BrevoMailer;
pub use log::{CapturedEmail, CapturingMailer, LogMailer};
pub use smtp::SmtpMailer;
