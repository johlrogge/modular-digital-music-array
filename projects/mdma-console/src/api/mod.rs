//! JSON API for the new web UI. Each submodule registers its own routes here.

use crate::AppState;
use axum::Router;
use std::sync::Arc;

// Wire types are pinned now (with tests) and consumed by the P1 routes.
#[allow(dead_code)]
pub mod library;
#[allow(dead_code)]
pub mod player;

/// All `/api/...` routes. Empty until P1 adds library and player routes.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
}
