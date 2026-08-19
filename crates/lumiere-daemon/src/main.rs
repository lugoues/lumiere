use std::{error::Error, fs, path::Path, process::ExitCode, time::Duration};

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

const USAGE: &str = "Usage: lumiere-daemon [OPTIONS]

Options:
  --sim              Serve four simulated lights instead of Bluetooth hardware
  --bind ADDR:PORT   Listen address for this run, overriding the config file
                     (example: --bind 127.0.0.1:9090)
  -h, --help         Show this help

The persistent listen address, auth token, and CORS origins live in the config
file printed at startup. Environment overrides: LUMIERE_CONFIG_DIR,
LUMIERE_DATA_DIR, LUMIERE_WEB_ROOT.";

async fn run() -> Result<(), Box<dyn Error>> {
    let mut sim = false;
    let mut bind = None;
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--sim" => sim = true,
            "--bind" => {
                let value = args.next().ok_or("--bind requires ADDR:PORT")?;
                bind = Some(value.parse().map_err(|_| {
                    format!(
                        "invalid --bind value {value:?}; expected ADDR:PORT like 127.0.0.1:9090"
                    )
                })?);
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            other => return Err(format!("unknown argument {other:?}\n\n{USAGE}").into()),
        }
    }

    let mut config = Config::load_or_create()?;
    if let Some(bind) = bind {
        config.bind = bind;
    }
    println!("Config file: {}", Config::path()?.display());
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
    let data_dir = store::default_dir()?;
    let animations_dir = data_dir.join("animations");
    seed_animations(&animations_dir)?;
    let stored = store::load(&data_dir)?;
    let (store_updates, store_task) = store::spawn(data_dir, stored.clone());
    let registry = RegistryHandle::spawn_with_config(
        transport,
        RegistryConfig {
            labels: stored.labels,
            presets: stored.presets,
            store_updates: Some(store_updates),
            animations_dir: Some(animations_dir),
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

fn seed_animations(destination: &Path) -> Result<(), Box<dyn Error>> {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/animations");
    fs::create_dir_all(destination)?;
    // Shipped animations are defaults: copy missing files, preserving every
    // local file so future user edits are never overwritten on startup.
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if !entry.file_type()?.is_file()
            || entry
                .path()
                .extension()
                .is_none_or(|extension| extension != "json")
        {
            continue;
        }
        let output = destination.join(entry.file_name());
        if !output.exists() {
            fs::copy(entry.path(), output)?;
        }
    }
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
