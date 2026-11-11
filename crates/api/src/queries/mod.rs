//! Reads of one tenant's events, answering the same questions gsd's SQLite
//! store answers locally, with the same results for the same events. Every
//! read deduplicates (`groundstation_backend::ch::deduplicated`).

pub mod stats;
pub mod trajectories;
