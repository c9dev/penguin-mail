//! The calendar page. Split so every decision that does not need a
//! widget lives in a plain module with its own tests, and a widget only
//! places what that module worked out.

pub mod block;
pub mod layout;
pub mod range;
pub mod time_grid;
pub mod tint;
