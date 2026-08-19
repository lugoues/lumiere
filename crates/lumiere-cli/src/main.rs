use std::{env, process::ExitCode, time::Duration};

use futures::StreamExt;
use lumiere_core::{ModelTable, clamp_to_device, encode};
use lumiere_proto::{Capabilities, Kelvin, Mode, Percent};
use lumiere_transport::{Discovered, Link, ScanFilter, Transport, WriteKind, ble::BleTransport};
use tokio::time::Instant;
use tracing_subscriber::EnvFilter;

const USAGE: &str = "Usage:
  lumiere probe scan [--seconds N]
  lumiere probe blink <id-or-name-fragment> [--seconds N]
  lumiere probe bench <id-or-name-fragment> [--writes N]";

enum Command {
    Scan { seconds: u64 },
    Blink { fragment: String, seconds: u64 },
    Bench { fragment: String, writes: usize },
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .try_init()
        .ok();

    let command = match parse_args(env::args().skip(1).collect()) {
        Ok(command) => command,
        Err(message) => {
            if !message.is_empty() {
                eprintln!("error: {message}\n");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(1);
        }
    };
    match run(command).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(2)
        }
    }
}

fn parse_args(args: Vec<String>) -> Result<Command, String> {
    let [namespace, subcommand, rest @ ..] = args.as_slice() else {
        return Err(String::new());
    };
    if namespace != "probe" {
        return Err(format!("unknown command {namespace:?}"));
    }
    match subcommand.as_str() {
        "scan" => Ok(Command::Scan {
            seconds: parse_option(rest, "--seconds", 10)?,
        }),
        "blink" => {
            let (fragment, options) = rest
                .split_first()
                .ok_or_else(|| "blink requires an id or name fragment".to_owned())?;
            if fragment.starts_with('-') {
                return Err("blink requires an id or name fragment before its options".into());
            }
            Ok(Command::Blink {
                fragment: fragment.clone(),
                seconds: parse_option(options, "--seconds", 6)?,
            })
        }
        "bench" => {
            let (fragment, options) = rest
                .split_first()
                .ok_or_else(|| "bench requires an id or name fragment".to_owned())?;
            if fragment.starts_with('-') {
                return Err("bench requires an id or name fragment before its options".into());
            }
            Ok(Command::Bench {
                fragment: fragment.clone(),
                writes: parse_option(options, "--writes", 50)?,
            })
        }
        _ => Err(format!("unknown probe command {subcommand:?}")),
    }
}

fn parse_option<T>(args: &[String], name: &str, default: T) -> Result<T, String>
where
    T: std::str::FromStr,
{
    if args.is_empty() {
        return Ok(default);
    }
    let [actual, value] = args else {
        return Err(format!("expected {name} N"));
    };
    if actual != name {
        return Err(format!("unknown option {actual:?}"));
    }
    value
        .parse()
        .map_err(|_| format!("invalid value {value:?} for {name}"))
}

async fn run(command: Command) -> Result<(), String> {
    let transport = BleTransport::new()
        .await
        .map_err(|error| error.to_string())?;
    match command {
        Command::Scan { seconds } => scan(&transport, seconds).await,
        Command::Blink { fragment, seconds } => blink(&transport, &fragment, seconds).await,
        Command::Bench { fragment, writes } => bench(&transport, &fragment, writes).await,
    }
}

async fn scan(transport: &BleTransport, seconds: u64) -> Result<(), String> {
    let mut scan = transport
        .scan(ScanFilter::default())
        .await
        .map_err(|error| error.to_string())?;
    let deadline = tokio::time::sleep(Duration::from_secs(seconds));
    tokio::pin!(deadline);
    println!("Scanning for {seconds} seconds...");
    loop {
        tokio::select! {
            _ = &mut deadline => return Ok(()),
            signal = tokio::signal::ctrl_c() => {
                signal.map_err(|error| format!("could not listen for Ctrl-C: {error}"))?;
                return Ok(());
            }
            discovered = scan.events.next() => match discovered {
                Some(discovered) => print_discovered(&discovered),
                None => return Err("Bluetooth event stream closed during scan".into()),
            }
        }
    }
}

fn print_discovered(discovered: &Discovered) {
    let name = discovered.name.as_deref().unwrap_or("<unknown>");
    let rssi = discovered
        .rssi
        .map_or_else(|| "n/a".into(), |rssi| format!("{rssi} dBm"));
    if let Some(name) = discovered.name.as_deref() {
        let caps = ModelTable::builtin().resolve(name);
        println!(
            "{}  {}  {}  [CCT {}-{} K, RGB {}]",
            discovered.id,
            name,
            rssi,
            caps.cct_min.get(),
            caps.cct_max.get(),
            if caps.rgb { "yes" } else { "no" }
        );
    } else {
        println!("{}  {}  {}", discovered.id, name, rssi);
    }
}

async fn select_light(transport: &BleTransport, fragment: &str) -> Result<Discovered, String> {
    let mut scan = transport
        .scan(ScanFilter::default())
        .await
        .map_err(|error| error.to_string())?;
    let deadline = tokio::time::sleep(Duration::from_secs(5));
    tokio::pin!(deadline);
    let mut candidates: Vec<Discovered> = Vec::new();
    loop {
        tokio::select! {
            _ = &mut deadline => break,
            discovered = scan.events.next() => match discovered {
                Some(discovered) => {
                    if let Some(existing) = candidates.iter_mut().find(|item| item.id == discovered.id) {
                        *existing = discovered;
                    } else {
                        candidates.push(discovered);
                    }
                }
                None => break,
            }
        }
    }
    drop(scan);
    let needle = fragment.to_lowercase();
    let matches: Vec<_> = candidates
        .iter()
        .filter(|candidate| {
            candidate.id.as_str().to_lowercase().contains(&needle)
                || candidate
                    .name
                    .as_ref()
                    .is_some_and(|name| name.to_lowercase().contains(&needle))
        })
        .cloned()
        .collect();
    match matches.as_slice() {
        [selected] => Ok(selected.clone()),
        [] => Err(format!(
            "no matching light found. Candidates: {}",
            candidate_list(&candidates)
        )),
        _ => Err(format!(
            "fragment is ambiguous. Matches: {}",
            candidate_list(&matches)
        )),
    }
}

fn candidate_list(candidates: &[Discovered]) -> String {
    if candidates.is_empty() {
        return "none".into();
    }
    candidates
        .iter()
        .map(|candidate| {
            format!(
                "{} ({})",
                candidate.id,
                candidate.name.as_deref().unwrap_or("unknown name")
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn capabilities(light: &Discovered) -> Capabilities {
    ModelTable::builtin().resolve(light.name.as_deref().unwrap_or(""))
}

async fn blink(transport: &BleTransport, fragment: &str, seconds: u64) -> Result<(), String> {
    println!("Scanning for a matching light...");
    let light = select_light(transport, fragment).await?;
    println!(
        "Connecting to {} ({})...",
        light.id,
        light.name.as_deref().unwrap_or("unknown name")
    );
    let caps = capabilities(&light);
    let link = transport
        .connect(&light.id, Duration::from_secs(10))
        .await
        .map_err(|error| error.to_string())?;
    for second in 0..seconds {
        let mode = if second.is_multiple_of(2) {
            Mode::On
        } else {
            Mode::Off
        };
        write_mode(link.as_ref(), mode, &caps, WriteKind::WithResponse).await?;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    write_mode(link.as_ref(), Mode::On, &caps, WriteKind::WithResponse).await?;
    link.disconnect().await.map_err(|error| error.to_string())?;
    println!("Blink complete; the light was left on.");
    Ok(())
}

async fn write_mode(
    link: &dyn Link,
    mode: Mode,
    caps: &Capabilities,
    kind: WriteKind,
) -> Result<(), String> {
    for packet in encode(mode, caps).packets() {
        link.write(packet.as_bytes(), kind)
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

async fn bench(transport: &BleTransport, fragment: &str, writes: usize) -> Result<(), String> {
    if writes == 0 {
        return Err("--writes must be greater than zero".into());
    }
    println!("Scanning for a matching light...");
    let light = select_light(transport, fragment).await?;
    println!(
        "Connecting to {} ({})...",
        light.id,
        light.name.as_deref().unwrap_or("unknown name")
    );
    let caps = capabilities(&light);
    let link = transport
        .connect(&light.id, Duration::from_secs(10))
        .await
        .map_err(|error| error.to_string())?;
    let without = benchmark(link.as_ref(), &caps, writes, WriteKind::WithoutResponse).await?;
    let with = benchmark(link.as_ref(), &caps, writes, WriteKind::WithResponse).await?;
    print_stats("WithoutResponse", &without);
    print_stats("WithResponse", &with);
    println!("Hint: the WithoutResponse throughput cap is what bounds animation fps.");
    link.disconnect().await.map_err(|error| error.to_string())?;
    Ok(())
}

async fn benchmark(
    link: &dyn Link,
    caps: &Capabilities,
    count: usize,
    kind: WriteKind,
) -> Result<Vec<Duration>, String> {
    let mut timings = Vec::new();
    for index in 0..count {
        let requested = Mode::Cct {
            temp: Kelvin::new(if index.is_multiple_of(2) { 3200 } else { 5600 })
                .expect("benchmark temperatures are valid"),
            bri: Percent::new(50).expect("benchmark brightness is valid"),
        };
        let (mode, _) = clamp_to_device(requested, caps);
        for packet in encode(mode, caps).packets() {
            let started = Instant::now();
            link.write(packet.as_bytes(), kind)
                .await
                .map_err(|error| error.to_string())?;
            timings.push(started.elapsed());
        }
    }
    Ok(timings)
}

fn print_stats(label: &str, timings: &[Duration]) {
    let mut sorted = timings.to_vec();
    sorted.sort_unstable();
    let total: Duration = sorted.iter().copied().sum();
    let mean = total / u32::try_from(sorted.len()).expect("benchmark sample count fits u32");
    let percentile = |percent: usize| sorted[((sorted.len() - 1) * percent).div_ceil(100)];
    let writes_per_second = sorted.len() as f64 / total.as_secs_f64();
    println!(
        "{label}: min {:?}, p50 {:?}, p95 {:?}, max {:?}, mean {:?}, {:.1} writes/sec",
        sorted[0],
        percentile(50),
        percentile(95),
        sorted[sorted.len() - 1],
        mean,
        writes_per_second
    );
}
