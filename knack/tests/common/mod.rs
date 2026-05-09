//! Shared helpers for integration tests.
//!
//! Each test in `tests/*.rs` becomes its own crate; they all `mod common;`
//! to pull this module in.

#![allow(dead_code)] // not every test exercises every helper

use std::sync::Arc;

use knack_cli::api::ApiClient;
use knack_cli::auth_store::{MemoryStore, StoredTokens, TokenStore};
use knack_cli::config::Config;
use wiremock::MockServer;

pub const FAKE_ACCESS_TOKEN: &str = "test-access-token";
pub const FAKE_REFRESH_TOKEN: &str = "test-refresh-token";

/// Spin up a wiremock server + an ApiClient pointed at it, with a memory
/// token store pre-seeded so authenticated calls work without a real device
/// flow.
pub async fn fixture() -> (MockServer, ApiClient, Arc<MemoryStore>) {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryStore::new());
    store
        .save(
            "default",
            &StoredTokens {
                access_token: FAKE_ACCESS_TOKEN.into(),
                refresh_token: FAKE_REFRESH_TOKEN.into(),
                expires_at: chrono::Utc::now().timestamp() + 3600,
            },
        )
        .expect("seed memory store");

    let mut config = Config::load();
    config.api_base = server.uri();

    let client = ApiClient::new(
        config,
        store.clone() as Arc<dyn TokenStore + Send + Sync>,
        "default",
    );
    (server, client, store)
}

/// Same as [`fixture`] but with no tokens preloaded — used by tests that want
/// to drive the device-flow login from scratch.
pub async fn fixture_unauth() -> (MockServer, ApiClient) {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryStore::new()) as Arc<dyn TokenStore + Send + Sync>;
    let mut config = Config::load();
    config.api_base = server.uri();
    let client = ApiClient::new(config, store, "default");
    (server, client)
}
