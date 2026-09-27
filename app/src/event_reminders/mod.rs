//! Event reminders: which ones are due and when to look again
//! ([`plan`]), and what each notification says ([`notice`]). Neither
//! touches GTK, the store or a clock; `app/src/app/reminders.rs` does
//! that around them.

pub mod notice;
pub mod plan;
