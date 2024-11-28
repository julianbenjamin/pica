use crate::{
    domain::{ConnectionsConfig, K8sMode, Metric},
    helper::{K8sDriver, K8sDriverImpl, K8sDriverLogger},
    logic::{connection_oauth_definition::FrontendOauthConnectionDefinition, openapi::OpenAPIData},
    router,
};
use anyhow::{anyhow, Context, Result};
use axum::Router;
use integrationos_cache::local::{
    connection_cache::ConnectionCacheArcStrHeaderKey,
    connection_definition_cache::ConnectionDefinitionCache,
    connection_oauth_definition_cache::ConnectionOAuthDefinitionCache,
    event_access_cache::EventAccessCache,
};
use integrationos_domain::{
    algebra::{DefaultTemplate, MongoStore},
    common_model::{CommonEnum, CommonModel},
    connection_definition::{ConnectionDefinition, PublicConnectionDetails},
    connection_model_definition::ConnectionModelDefinition,
    connection_model_schema::{ConnectionModelSchema, PublicConnectionModelSchema},
    connection_oauth_definition::{ConnectionOAuthDefinition, Settings},
    cursor::Cursor,
    event_access::EventAccess,
    page::PlatformPage,
    secret::Secret,
    secrets::SecretServiceProvider,
    stage::Stage,
    user::UserClient,
    Connection, Event, GoogleKms, IOSKms, Pipeline, PlatformData, PublicConnection, SecretExt,
    Store, Transaction,
};
use integrationos_unified::unified::{UnifiedCacheTTLs, UnifiedDestination};
use mongodb::{options::UpdateOptions, Client, Database};
use segment::{AutoBatcher, Batcher, HttpClient};
use std::{sync::Arc, time::Duration};
use tokio::{net::TcpListener, sync::mpsc::Sender, time::timeout, try_join};
use tracing::{error, info, trace, warn};

#[derive(Clone)]
pub struct AppStores {
    pub clients: MongoStore<UserClient>,
    pub common_enum: MongoStore<CommonEnum>,
    pub common_model: MongoStore<CommonModel>,
    pub connection: MongoStore<Connection>,
    pub connection_config: MongoStore<ConnectionDefinition>,
    pub cursors: MongoStore<Cursor>,
    pub db: Database,
    pub event: MongoStore<Event>,
    pub event_access: MongoStore<EventAccess>,
    pub frontend_oauth_config: MongoStore<FrontendOauthConnectionDefinition>,
    pub model_config: MongoStore<ConnectionModelDefinition>,
    pub model_schema: MongoStore<ConnectionModelSchema>,
    pub oauth_config: MongoStore<ConnectionOAuthDefinition>,
    pub pipeline: MongoStore<Pipeline>,
    pub platform: MongoStore<PlatformData>,
    pub platform_page: MongoStore<PlatformPage>,
    pub public_connection: MongoStore<PublicConnection>,
    pub public_connection_details: MongoStore<PublicConnectionDetails>,
    pub public_model_schema: MongoStore<PublicConnectionModelSchema>,
    pub secrets: MongoStore<Secret>,
    pub settings: MongoStore<Settings>,
    pub stages: MongoStore<Stage>,
    pub transactions: MongoStore<Transaction>,
}

#[derive(Clone)]
pub struct AppState {
    pub app_stores: AppStores,
    pub config: ConnectionsConfig,
    pub connection_definitions_cache: ConnectionDefinitionCache,
    pub connection_oauth_definitions_cache: ConnectionOAuthDefinitionCache,
    pub connections_cache: ConnectionCacheArcStrHeaderKey,
    pub event_access_cache: EventAccessCache,
    pub event_tx: Sender<Event>,
    pub extractor_caller: UnifiedDestination,
    pub http_client: reqwest::Client,
    pub k8s_client: Arc<dyn K8sDriver>,
    pub metric_tx: Sender<Metric>,
    pub openapi_data: OpenAPIData,
    pub secrets_client: Arc<dyn SecretExt + Sync + Send>,
    pub template: DefaultTemplate,
}

#[derive(Clone)]
pub struct Server {
    state: Arc<AppState>,
}

impl Server {
    pub async fn init(config: ConnectionsConfig) -> Result<Self> {
        let client = Client::with_uri_str(&config.db_config.control_db_url).await?;
        let db = client.database(&config.db_config.control_db_name);

        let http_client = reqwest::ClientBuilder::new()
            .timeout(Duration::from_secs(config.http_client_timeout_secs))
            .build()?;
        let model_config = MongoStore::new(&db, &Store::ConnectionModelDefinitions).await?;
        let oauth_config = MongoStore::new(&db, &Store::ConnectionOAuthDefinitions).await?;
        let frontend_oauth_config =
            MongoStore::new(&db, &Store::ConnectionOAuthDefinitions).await?;
        let model_schema = MongoStore::new(&db, &Store::ConnectionModelSchemas).await?;
        let public_model_schema =
            MongoStore::new(&db, &Store::PublicConnectionModelSchemas).await?;
        let common_model = MongoStore::new(&db, &Store::CommonModels).await?;
        let common_enum = MongoStore::new(&db, &Store::CommonEnums).await?;
        let secrets = MongoStore::new(&db, &Store::Secrets).await?;
        let connection = MongoStore::new(&db, &Store::Connections).await?;
        let public_connection = MongoStore::new(&db, &Store::Connections).await?;
        let platform = MongoStore::new(&db, &Store::Platforms).await?;
        let platform_page = MongoStore::new(&db, &Store::PlatformPages).await?;
        let public_connection_details =
            MongoStore::new(&db, &Store::PublicConnectionDetails).await?;
        let settings = MongoStore::new(&db, &Store::Settings).await?;
        let connection_config = MongoStore::new(&db, &Store::ConnectionDefinitions).await?;
        let pipeline = MongoStore::new(&db, &Store::Pipelines).await?;
        let event_access = MongoStore::new(&db, &Store::EventAccess).await?;
        let event = MongoStore::new(&db, &Store::Events).await?;
        let transactions = MongoStore::new(&db, &Store::Transactions).await?;
        let cursors = MongoStore::new(&db, &Store::Cursors).await?;
        let stages = MongoStore::new(&db, &Store::Stages).await?;
        let clients = MongoStore::new(&db, &Store::Clients).await?;
        let secrets_store = MongoStore::<Secret>::new(&db, &Store::Secrets).await?;

        let secrets_client: Arc<dyn SecretExt + Sync + Send> = match config.secrets_config.provider
        {
            SecretServiceProvider::GoogleKms => {
                Arc::new(GoogleKms::new(&config.secrets_config, secrets_store).await?)
            }
            SecretServiceProvider::IosKms => {
                Arc::new(IOSKms::new(&config.secrets_config, secrets_store).await?)
            }
        };

        let extractor_caller = UnifiedDestination::new(
            config.db_config.clone(),
            config.cache_size,
            secrets_client.clone(),
            UnifiedCacheTTLs {
                connection_cache_ttl_secs: config.connection_cache_ttl_secs,
                connection_model_schema_cache_ttl_secs: config
                    .connection_model_schema_cache_ttl_secs,
                connection_model_definition_cache_ttl_secs: config
                    .connection_model_definition_cache_ttl_secs,
                secret_cache_ttl_secs: config.secret_cache_ttl_secs,
            },
        )
        .await
        .with_context(|| "Could not initialize extractor caller")?;

        let app_stores = AppStores {
            db: db.clone(),
            model_config,
            oauth_config,
            platform_page,
            frontend_oauth_config,
            secrets,
            model_schema,
            public_model_schema,
            platform,
            settings,
            common_model,
            common_enum,
            connection,
            public_connection,
            public_connection_details,
            connection_config,
            pipeline,
            event_access,
            event,
            transactions,
            cursors,
            stages,
            clients,
        };

        let event_access_cache =
            EventAccessCache::new(config.cache_size, config.access_key_cache_ttl_secs);
        let connections_cache = ConnectionCacheArcStrHeaderKey::create(
            config.cache_size,
            config.connection_cache_ttl_secs,
        );
        let connection_definitions_cache = ConnectionDefinitionCache::new(
            config.cache_size,
            config.connection_definition_cache_ttl_secs,
        );
        let connection_oauth_definitions_cache = ConnectionOAuthDefinitionCache::new(
            config.cache_size,
            config.connection_oauth_definition_cache_ttl_secs,
        );
        let openapi_data = OpenAPIData::default();
        openapi_data.spawn_openapi_generation(
            app_stores.common_model.clone(),
            app_stores.common_enum.clone(),
        );

        let k8s_client: Arc<dyn K8sDriver> = match config.k8s_mode {
            K8sMode::Real => Arc::new(K8sDriverImpl::new().await?),
            K8sMode::Logger => Arc::new(K8sDriverLogger),
        };

        // Create Event buffer in separate thread and batch saves
        let events = db.collection::<Event>(&Store::Events.to_string());
        let (event_tx, mut receiver) =
            tokio::sync::mpsc::channel::<Event>(config.event_save_buffer_size);
        tokio::spawn(async move {
            let mut buffer = Vec::with_capacity(config.event_save_buffer_size);
            loop {
                let res = timeout(
                    Duration::from_secs(config.event_save_timeout_secs),
                    receiver.recv(),
                )
                .await;
                let is_timeout = if let Ok(Some(event)) = res {
                    buffer.push(event);
                    false
                } else if let Ok(None) = res {
                    break;
                } else {
                    trace!("Event receiver timed out waiting for new event");
                    true
                };
                // Save when buffer is full or timeout elapsed
                if buffer.len() == config.event_save_buffer_size
                    || (is_timeout && !buffer.is_empty())
                {
                    trace!("Saving {} events", buffer.len());
                    let to_save = std::mem::replace(
                        &mut buffer,
                        Vec::with_capacity(config.event_save_buffer_size),
                    );
                    let events = events.clone();
                    tokio::spawn(async move {
                        if let Err(e) = events.insert_many(to_save).await {
                            error!("Could not save buffer of events: {e}");
                        }
                    });
                }
            }
        });

        // Update metrics in separate thread
        let client = HttpClient::default();
        let batcher = Batcher::new(None);
        let template = DefaultTemplate::default();
        let mut batcher = config
            .segment_write_key
            .as_ref()
            .map(|k| AutoBatcher::new(client, batcher, k.to_string()));

        let metrics = db.collection::<Metric>(&Store::Metrics.to_string());
        let (metric_tx, mut receiver) =
            tokio::sync::mpsc::channel::<Metric>(config.metric_save_channel_size);
        let metric_system_id = config.metric_system_id.clone();
        tokio::spawn(async move {
            let options = UpdateOptions::builder().upsert(true).build();

            loop {
                let res = timeout(
                    Duration::from_secs(config.event_save_timeout_secs),
                    receiver.recv(),
                )
                .await;
                if let Ok(Some(metric)) = res {
                    let doc = metric.update_doc();
                    let client = metrics
                        .update_one(
                            bson::doc! {
                                "clientId": &metric.ownership().client_id,
                            },
                            doc.clone(),
                        )
                        .with_options(options.clone());
                    let system = metrics
                        .update_one(
                            bson::doc! {
                                "clientId": metric_system_id.as_str(),
                            },
                            doc,
                        )
                        .with_options(options.clone());
                    if let Err(e) = try_join!(client, system) {
                        error!("Could not upsert metric: {e}");
                    }

                    if let Some(ref mut batcher) = batcher {
                        let msg = metric.segment_track();
                        if let Err(e) = batcher.push(msg).await {
                            warn!("Tracking msg is too large: {e}");
                        }
                    }
                } else if let Ok(None) = res {
                    break;
                } else {
                    trace!("Event receiver timed out waiting for new event");
                    if let Some(ref mut batcher) = batcher {
                        if let Err(e) = batcher.flush().await {
                            warn!("Tracking flush is too large: {e}");
                        }
                    }
                }
            }
            if let Some(ref mut batcher) = batcher {
                if let Err(e) = batcher.flush().await {
                    warn!("Tracking flush is too large: {e}");
                }
            }
        });

        Ok(Self {
            state: Arc::new(AppState {
                app_stores,
                config,
                connection_definitions_cache,
                connection_oauth_definitions_cache,
                connections_cache,
                event_access_cache,
                event_tx,
                extractor_caller,
                http_client,
                k8s_client,
                metric_tx,
                openapi_data,
                secrets_client,
                template,
            }),
        })
    }

    pub async fn run(&self) -> Result<()> {
        let app = router::get_router(&self.state).await;

        let app: Router<()> = app.with_state(self.state.clone());

        info!("Api server listening on {}", self.state.config.address);

        let tcp_listener = TcpListener::bind(&self.state.config.address).await?;

        axum::serve(tcp_listener, app.into_make_service())
            .await
            .map_err(|e| anyhow!("Server error: {}", e))
    }
}
