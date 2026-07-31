//! Shared subscription selection, bounded fetching, and normalization for RSS tools.

use std::time::Duration;

use anyhow::{Context, Result};

mod fetch;
mod model;
mod subscription;

pub use fetch::{FetchFailure, fetch_subscriptions, parse_feed};
pub use model::{FetchedEntry, FetchedFeed};
pub use subscription::{Subscription, select_subscriptions};

/// Default number of recent entries fetched from each subscription.
pub const DEFAULT_ENTRY_LIMIT: usize = 20;

/// Build the bounded HTTP client shared by RSS snapshot and sync commands.
///
/// # Errors
///
/// Returns an error when the underlying HTTP client cannot be constructed.
pub fn build_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("clix/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(45))
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .context("failed to build RSS HTTP client")
}
