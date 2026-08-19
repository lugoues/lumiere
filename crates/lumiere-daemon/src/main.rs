use std::{process::ExitCode, time::Duration};

use lumiere_daemon::RegistryHandle;
use lumiere_proto::LightId;
use lumiere_transport::sim::{SimConfig, SimLightSpec, SimTransport};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    if !std::env::args().any(|argument| argument == "--sim") {
        eprintln!("real transport not yet implemented");
        return ExitCode::FAILURE;
    }

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
    let transport = SimTransport::new(SimConfig {
        lights,
        write_latency: Duration::ZERO,
        fail_every_nth_write: None,
    });
    let registry = RegistryHandle::spawn(transport);
    let mut events = registry.events();
    registry
        .discover(Duration::from_secs(10))
        .await
        .expect("registry must accept discovery");

    loop {
        tokio::select! {
            result = events.recv() => match result {
                Ok(event) => info!(?event, "registry event"),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    info!(skipped, "event logger lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            _ = tokio::signal::ctrl_c() => break,
        }
    }

    registry.shutdown().await;
    ExitCode::SUCCESS
}
