use super::{read, CrudRequest};
use crate::server::{AppState, AppStores};
use axum::{routing::get, Router};
use bson::doc;
use integrationos_domain::{algebra::MongoStore, Event};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub fn get_router() -> Router<Arc<AppState>> {
    Router::new().route("/", get(read::<CreateEventRequest, Event>))
}

#[derive(Serialize, Deserialize)]
pub struct CreateEventRequest;

impl CrudRequest for CreateEventRequest {
    type Output = Event;

    fn get_store(stores: AppStores) -> MongoStore<Self::Output> {
        stores.event
    }
}
