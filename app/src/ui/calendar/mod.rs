//! The calendar page. Split so every decision that does not need a
//! widget lives in a plain module with its own tests, and a widget only
//! places what that module worked out.

pub mod agenda;
pub mod block;
pub mod layout;
pub mod month;
pub mod popover;
pub mod range;
pub mod sidebar;
pub mod time_grid;
pub mod tint;
pub mod words;
