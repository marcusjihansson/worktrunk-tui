//! wt-tui's library surface.
//!
//! The binary owns the terminal and the event loop; everything that reasons
//! about worktrunk lives here so it can be tested directly. In particular
//! [`wt`] is public because the contract test validates our model against the
//! JSON Schema worktrunk publishes.

pub mod app;
pub mod search;
pub mod ui;
pub mod watch;
pub mod wt;
