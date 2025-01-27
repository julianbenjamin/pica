mod client;
mod config;

use crate::client::WatchdogClient;
use anyhow::{Context, Result};
use config::WatchdogConfig;
use dotenvy::dotenv;
use entities::{
    cache::CacheConfig,
    database::DatabaseConfig,
    telemetry::{get_subscriber, init_subscriber},
};
use envconfig::Envconfig;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();

    let suscriber = get_subscriber("watchdog".into(), "info".into(), std::io::stdout, None);
    init_subscriber(suscriber);

    let watchdog_config = WatchdogConfig::init_from_env().context("Could not load config")?;
    let cache_config = CacheConfig::init_from_env().context("Could not load config")?;
    let database_config = DatabaseConfig::init_from_env().context("Could not load config")?;

    info!("Starting watchdog with config: {watchdog_config}{cache_config}{database_config}");

    let client = WatchdogClient::new(watchdog_config, cache_config, database_config);

    client.start().await?;

    Ok(())
}
