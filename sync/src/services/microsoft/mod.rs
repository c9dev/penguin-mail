//! The Microsoft adapter: every service a Microsoft account offers, over
//! Graph. This module starts as the seam; the adapter itself comes later.

mod api;

pub use api::GraphApi;
