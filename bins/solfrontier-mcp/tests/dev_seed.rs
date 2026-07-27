//! Ignored development entry point for preparing an MCP smoke-test database.
//!
//! This integration-test target is never part of `cargo build --release`.

#[path = "../src/dev_seed.rs"]
mod dev_seed;

use claw_state_store::{Database, DatabaseConfig};

#[tokio::test]
#[ignore = "development-only database seeder; run explicitly with SOLFRONTIER_DB"]
async fn seed_mcp_smoke_database() {
    let path =
        std::env::var("SOLFRONTIER_DB").expect("set SOLFRONTIER_DB to the SQLite file to seed");
    let db = Database::open(&DatabaseConfig {
        path,
        max_connections: 1,
    })
    .await
    .expect("development database must open");
    let intent = dev_seed::seed_intent_status_fixture(&db)
        .await
        .expect("development intent fixture must seed");

    println!(
        "{}",
        serde_json::json!({
            "intent_ref": dev_seed::DEV_INTENT_UUID,
            "intent_id": intent.intent_id,
            "canonical_rule_hash": intent.canonical_rule_hash_hex,
            "status": intent.status.as_str(),
            "amount_raw": intent.amount_raw,
        })
    );

    db.pool().close().await;
}
