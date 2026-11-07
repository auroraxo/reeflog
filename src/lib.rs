//! The library face of reeflog.
//!
//! Only the pieces a caller outside the binary needs are exported. Right now
//! that is `flags` — not because anything consumes it as a library, but
//! because the contract this module enforces (a flag the command does not
//! know is a refusal, not a silent drop) is checked against the DOCUMENT by
//! `tests/documented_surface.rs`, which has to reach the same code the binary
//! runs. Testing a reimplementation of the parser would prove the
//! reimplementation works.
//!
//! The rest of the product stays behind the CLI: the journal, the daemon and
//! the webhook poster are not stable API and are deliberately not exported.

pub mod flags;
