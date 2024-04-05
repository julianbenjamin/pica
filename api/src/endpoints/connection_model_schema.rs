use super::{
    create, delete, read, update, ApiError, ApiResult, CrudHook, CrudRequest, ReadResponse,
};
use crate::{
    api_payloads::ErrorResponse,
    internal_server_error,
    server::{AppState, AppStores},
    util::shape_mongo_filter,
};
use axum::{
    extract::{Path, Query, State},
    routing::{patch, post},
    Extension, Json, Router,
};
use http::StatusCode;
use integrationos_domain::{
    algebra::adapter::StoreAdapter,
    common::{
        connection_model_schema::{
            ConnectionModelSchema, Mappings, PublicConnectionModelSchema, SchemaPaths,
        },
        event_access::EventAccess,
        json_schema::JsonSchema,
        mongo::MongoDbStore,
    },
    id::{prefix::IdPrefix, Id},
};
use mongodb::bson::doc;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};
use tokio::try_join;
use tracing::error;

pub fn get_router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/",
            post(create::<CreateRequest, ConnectionModelSchema>)
                .get(read::<CreateRequest, ConnectionModelSchema>),
        )
        .route(
            "/:id",
            patch(update::<CreateRequest, ConnectionModelSchema>)
                .delete(delete::<CreateRequest, ConnectionModelSchema>),
        )
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "dummy", derive(fake::Dummy))]
#[serde(rename_all = "camelCase")]
pub struct PublicGetConnectionModelSchema {
    pub connection_definition_id: Id,
}

pub async fn public_get_connection_model_schema<T, U>(
    event_access: Option<Extension<Arc<EventAccess>>>,
    query: Option<Query<BTreeMap<String, String>>>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<ReadResponse<U>>, ApiError>
where
    T: CrudRequest<Output = U> + 'static,
    U: Serialize + DeserializeOwned + Unpin + Sync + Send + 'static,
{
    match query.as_ref().and_then(|q| q.get("connectionDefinitionId")) {
        Some(id) => id.to_string(),
        None => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "connectionDefinitionId is required".to_string(),
                }),
            ))
        }
    };

    let mut query = shape_mongo_filter(
        query,
        event_access.map(|e| {
            let Extension(e) = e;
            e
        }),
        None,
    );

    query.filter.remove("ownership.buildableId");
    query.filter.remove("environment");
    query.filter.insert("mapping", doc! { "$ne": null });

    let store = T::get_store(state.app_stores.clone());
    let count = store.count(query.filter.clone(), None);
    let find = store.get_many(
        Some(query.filter),
        None,
        None,
        Some(query.limit),
        Some(query.skip),
    );

    let res = match try_join!(count, find) {
        Ok((total, rows)) => ReadResponse {
            rows,
            skip: query.skip,
            limit: query.limit,
            total,
        },
        Err(e) => {
            error!("Error reading from store: {e}");
            return Err(internal_server_error!());
        }
    };

    Ok(Json(res))
}

pub async fn public_get_platform_models(
    Path(platform_name): Path<String>,
    State(state): State<Arc<AppState>>,
) -> ApiResult<Vec<String>> {
    let store = state.app_stores.public_model_schema.clone();

    let res = store
        .get_many(
            Some(doc! {
                "connectionPlatform": &platform_name,
                "mapping": { "$ne": null }
            }),
            None,
            None,
            Some(100),
            None,
        )
        .await
        .map_err(|e| {
            error!("Error reading from connection model schema store: {e}");
            internal_server_error!()
        })?;

    let common_model_names = res
        .into_iter()
        .map(|r| r.mapping)
        .map(|m| m.common_model_name)
        .collect::<Vec<String>>();

    Ok(Json(common_model_names))
}

impl CrudRequest for PublicGetConnectionModelSchema {
    type Output = PublicConnectionModelSchema;
    type Error = ();

    fn into_public(self) -> Result<Self::Output, Self::Error> {
        unimplemented!()
    }

    fn into_with_event_access(self, _event_access: Arc<EventAccess>) -> Self::Output {
        unimplemented!()
    }

    fn update(self, _record: &mut Self::Output) {
        unimplemented!()
    }

    fn get_store(stores: AppStores) -> MongoDbStore<Self::Output> {
        stores.public_model_schema.clone()
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "dummy", derive(fake::Dummy))]
#[serde(rename_all = "camelCase")]
pub struct CreateRequest {
    pub platform_id: Id,
    pub platform_page_id: Id,
    pub connection_platform: String,
    pub connection_definition_id: Id,
    pub platform_version: String,
    pub model_name: String,
    pub schema: JsonSchema,
    pub sample: Value,
    pub paths: Option<SchemaPaths>,
    #[cfg_attr(feature = "dummy", dummy(default))]
    pub mapping: Option<Mappings>,
}

impl CrudHook<ConnectionModelSchema> for CreateRequest {}

impl CrudRequest for CreateRequest {
    type Output = ConnectionModelSchema;
    type Error = ();

    fn into_public(self) -> Result<Self::Output, Self::Error> {
        let key = format!(
            "api::{}::{}::{}",
            self.connection_platform, self.platform_version, self.model_name
        )
        .to_lowercase();

        Ok(Self::Output {
            id: Id::now(IdPrefix::ConnectionModelSchema),
            platform_id: self.platform_id,
            platform_page_id: self.platform_page_id,
            connection_platform: self.connection_platform,
            connection_definition_id: self.connection_definition_id,
            platform_version: self.platform_version,
            key,
            model_name: self.model_name,
            schema: self.schema,
            mapping: self.mapping,
            sample: self.sample,
            paths: self.paths,
            record_metadata: Default::default(),
        })
    }

    fn into_with_event_access(self, _event_access: Arc<EventAccess>) -> Self::Output {
        unimplemented!()
    }

    fn update(self, record: &mut Self::Output) {
        record.platform_id = self.platform_id;
        record.platform_page_id = self.platform_page_id;
        record.connection_platform = self.connection_platform;
        record.connection_definition_id = self.connection_definition_id;
        record.platform_version = self.platform_version;
        record.model_name = self.model_name;
        record.schema = self.schema;
        record.sample = self.sample;
        record.paths = self.paths;
        record.mapping = self.mapping;
    }

    fn get_store(stores: AppStores) -> MongoDbStore<Self::Output> {
        stores.model_schema.clone()
    }
}
