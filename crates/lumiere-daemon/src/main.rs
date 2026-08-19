use std::{error::Error, process::ExitCode, time::Duration};

use lumiere_daemon::{
    RegistryConfig, RegistryHandle,
    api::{ApiState, router},
    config::Config,
    store,
};
use lumiere_proto::LightId;
use lumiere_transport::{
    Transport,
    ble::BleTransport,
    sim::{SimConfig, SimLightSpec, SimTransport},
};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn Error>> {
    let config = Config::load_or_create()?;
    let sim = std::env::args().any(|argument| argument == "--sim");
    if sim {
        serve(sim_transport(), config).await
    } else {
        serve(BleTransport::new().await?, config).await
    }
}

async fn serve<T>(transport: T, config: Config) -> Result<(), Box<dyn Error>>
where
    T: Transport,
{
    let store_path = store::default_path()?;
    let labels = store::load(&store_path)?;
    let (store_updates, store_task) = store::spawn(store_path, labels.clone());
    let registry = RegistryHandle::spawn_with_config(
        transport,
        RegistryConfig {
            labels,
            store_updates: Some(store_updates),
            ..RegistryConfig::default()
        },
    );
    registry.discover(Duration::from_secs(10)).await?;

    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    let bound = listener.local_addr()?;
    println!("Lumière token: {}", config.token);
    println!("Bootstrap URL: http://{bound}/#t={}", config.token);

    let app = router(ApiState::new(registry.clone(), &config));
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await;

    registry.shutdown().await;
    drop(registry);
    store_task.await??;
    result?;
    Ok(())
}

fn sim_transport() -> SimTransport {
    let names = [
        "NEEWER-RGB660 PRO",
        "NEEWER-SNL660",
        "NEEWER-RGB176",
        "NEEWER-SL80",
    ];
    let lights = names
        .iter()
        .enumerate()
        .map(|(index, name)| SimLightSpec {
            id: LightId::sim(&(index + 1).to_string()),
            advertised_name: (*name).to_owned(),
            rssi: -40 - index as i16,
            connect_failures: 0,
        })
        .collect();
    SimTransport::new(SimConfig {
        lights,
        write_latency: Duration::ZERO,
        fail_every_nth_write: None,
    })
}
