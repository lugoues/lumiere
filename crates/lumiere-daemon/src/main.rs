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
    let args = match Args::parse() {
        Ok(Some(args)) => args,
        Ok(None) => return ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let config = Config::load_or_create();
    let config_log = config
        .as_ref()
        .ok()
        .and_then(|config| config.log.as_deref());
    if let Err(error) = init_tracing(args.log.as_deref(), config_log) {
        eprintln!("invalid log filter: {error}");
        return ExitCode::FAILURE;
    }
    let config = match config {
        Ok(config) => config,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };

    match run(args, config).await {
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
  --log FILTER       Tracing filter for this run (example: debug or
                     lumiere_daemon=debug,lumiere_transport=trace)
  --disable-authentication
                     Serve the API without bearer-token checks for this run.
                     Anyone who can reach the port controls the lights; meant
                     for trusted networks like a tailnet or loopback.
                     Set disable_authentication = true in the config file to
                     make it permanent (e.g. for the brew service).
  -h, --help         Show this help

The persistent listen address, auth token, and CORS origins live in the config
file printed at startup. Environment overrides: LUMIERE_CONFIG_DIR,
LUMIERE_DATA_DIR, LUMIERE_WEB_ROOT. RUST_LOG sets the tracing filter.";

struct Args {
    sim: bool,
    bind: Option<std::net::SocketAddr>,
    log: Option<String>,
    disable_authentication: bool,
}

impl Args {
    fn parse() -> Result<Option<Self>, Box<dyn Error>> {
        let mut parsed = Self {
            sim: false,
            bind: None,
            log: None,
            disable_authentication: false,
        };
        let mut args = std::env::args().skip(1);
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--sim" => parsed.sim = true,
                "--bind" => {
                    let value = args.next().ok_or("--bind requires ADDR:PORT")?;
                    parsed.bind = Some(value.parse().map_err(|_| {
                        format!(
                            "invalid --bind value {value:?}; expected ADDR:PORT like 127.0.0.1:9090"
                        )
                    })?);
                }
                "--log" => parsed.log = Some(args.next().ok_or("--log requires FILTER")?),
                "--disable-authentication" => parsed.disable_authentication = true,
                "-h" | "--help" => {
                    println!("{USAGE}");
                    return Ok(None);
                }
                other => return Err(format!("unknown argument {other:?}\n\n{USAGE}").into()),
            }
        }
        Ok(Some(parsed))
    }
}

fn init_tracing(flag: Option<&str>, config: Option<&str>) -> Result<(), Box<dyn Error>> {
    let env = std::env::var("RUST_LOG").ok();
    let directive = flag.or(env.as_deref()).or(config).unwrap_or("info");
    let filter = EnvFilter::try_new(directive)?;
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
    Ok(())
}

async fn run(args: Args, mut config: Config) -> Result<(), Box<dyn Error>> {
    if let Some(bind) = args.bind {
        config.bind = bind;
    }
    println!("Config file: {}", Config::path()?.display());
    if args.sim {
        serve(sim_transport(), config, args.disable_authentication).await
    } else {
        serve(
            BleTransport::new().await?,
            config,
            args.disable_authentication,
        )
        .await
    }
}

async fn serve<T>(
    transport: T,
    config: Config,
    disable_authentication: bool,
) -> Result<(), Box<dyn Error>>
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
            saved: stored.lights.clone(),
            presets: stored.presets,
            store_updates: Some(store_updates),
            animations_dir: Some(animations_dir),
            ..RegistryConfig::default()
        },
    );
    registry.discover(Duration::from_secs(10)).await?;

    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    let bound = listener.local_addr()?;
    let flag = disable_authentication;
    let disable_authentication = flag || config.disable_authentication;
    let state = if disable_authentication {
        let source = if flag {
            "--disable-authentication"
        } else {
            "disable_authentication in config.toml"
        };
        tracing::warn!("authentication is DISABLED: anyone reaching this port controls the lights");
        println!("Authentication: DISABLED ({source})");
        println!("Open: http://{bound}/");
        ApiState::new(registry.clone(), &config).without_authentication()
    } else {
        println!("Lumière token: {}", config.token);
        println!("Bootstrap URL: http://{bound}/#t={}", config.token);
        ApiState::new(registry.clone(), &config)
    };

    let app = router(state);
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

/// The animation library ships inside the binary: an installed daemon has no
/// source checkout to copy from.
#[derive(rust_embed::RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/../../assets/animations/"]
#[include = "*.json"]
struct ShippedAnimations;

fn seed_animations(destination: &Path) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(destination)?;
    // Shipped animations are defaults: write missing files, preserving every
    // local file so user edits are never overwritten on startup.
    for name in ShippedAnimations::iter() {
        let output = destination.join(name.as_ref());
        if !output.exists()
            && let Some(file) = ShippedAnimations::get(&name)
        {
            fs::write(output, file.data)?;
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
