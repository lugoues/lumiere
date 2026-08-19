use std::{
    collections::{BTreeMap, HashMap},
    env, fs, io,
    num::NonZeroU8,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use lumiere_core::schedule;
use lumiere_proto::{
    AnimTarget, Animation, AnimationId, Hue, Kelvin, Keyframe, Mode, Percent, PlaybackOptions,
};
use serde::Deserialize;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let Some(task) = args.next() else {
        return Err("usage: cargo xtask <ui [--debug] | convert-anims>".into());
    };
    if task == "convert-anims" {
        if let Some(extra) = args.next() {
            return Err(format!("unexpected argument {extra:?}"));
        }
        return convert_animations();
    }
    if task != "ui" {
        return Err(format!(
            "unknown task {task:?}; available tasks: ui, convert-anims"
        ));
    }

    let debug = match args.next() {
        None => false,
        Some(flag) if flag == "--debug" => true,
        Some(flag) => return Err(format!("unknown ui option {flag:?}")),
    };
    if let Some(extra) = args.next() {
        return Err(format!("unexpected argument {extra:?}"));
    }

    build_and_sync_ui(debug)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceAnimation {
    name: String,
    description: String,
    #[serde(rename = "loop")]
    looping: bool,
    keyframes: Vec<SourceKeyframe>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceKeyframe {
    hold_ms: u32,
    fade_ms: u32,
    lights: BTreeMap<String, SourceParams>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceParams {
    mode: String,
    hue: Option<i64>,
    sat: Option<i64>,
    bri: Option<i64>,
    temp: Option<i64>,
}

fn convert_animations() -> Result<(), String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source = root.join("references/NeewerLux/light_prefs/animations");
    let destination = root.join("assets/animations");
    fs::create_dir_all(&destination)
        .map_err(|error| format!("could not create {}: {error}", destination.display()))?;

    let mut paths = fs::read_dir(&source)
        .map_err(|error| format!("could not read {}: {error}", source.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("could not enumerate {}: {error}", source.display()))?;
    paths.retain(|path| {
        path.extension()
            .is_some_and(|extension| extension == "json")
    });
    paths.sort();

    let mut slugs = HashMap::<String, PathBuf>::new();
    let mut count = 0;
    for path in paths {
        let encoded = fs::read_to_string(&path)
            .map_err(|error| format!("{}: could not read: {error}", path.display()))?;
        let source_anim: SourceAnimation = serde_json::from_str(&encoded)
            .map_err(|error| format!("{}: invalid source JSON: {error}", path.display()))?;
        let slug = slug(&source_anim.name);
        let id =
            AnimationId::parse(&slug).map_err(|error| format!("{}: {error}", path.display()))?;
        if let Some(previous) = slugs.insert(slug.clone(), path.clone()) {
            return Err(format!(
                "slug collision {slug:?}: {} and {}",
                previous.display(),
                path.display()
            ));
        }

        let mut slot_count = 0;
        let mut keyframes = Vec::with_capacity(source_anim.keyframes.len());
        for (keyframe_index, source_keyframe) in source_anim.keyframes.into_iter().enumerate() {
            let mut lights = BTreeMap::new();
            let mut has_all = false;
            let mut has_slot = false;
            for (key, params) in source_keyframe.lights {
                let target = if key == "*" {
                    has_all = true;
                    AnimTarget::All
                } else {
                    let slot = key
                        .parse::<u8>()
                        .map_err(|_| format!("{}: unexpected light key {key:?}", path.display()))?;
                    if !(1..=4).contains(&slot) {
                        return Err(format!(
                            "{}: unexpected light key {key:?}; expected \"*\" or \"1\"..\"4\"",
                            path.display()
                        ));
                    }
                    has_slot = true;
                    slot_count = slot_count.max(slot);
                    AnimTarget::Slot(NonZeroU8::new(slot).expect("slot is nonzero"))
                };
                let mode = convert_mode(&path, keyframe_index, params)?;
                lights.insert(target, mode);
            }
            if has_all && has_slot {
                return Err(format!(
                    "{}: keyframe {keyframe_index} mixes all and slot targets",
                    path.display()
                ));
            }
            keyframes.push(Keyframe {
                hold_ms: source_keyframe.hold_ms,
                fade_ms: source_keyframe.fade_ms,
                lights,
            });
        }

        let animation = Animation {
            id,
            name: source_anim.name,
            description: source_anim.description,
            loop_default: source_anim.looping,
            slot_count,
            keyframes,
        };
        animation.validate().map_err(|error| {
            format!(
                "{}: converted animation is invalid: {error}",
                path.display()
            )
        })?;
        let options = PlaybackOptions {
            max_loops: u32::from(animation.loop_default),
            ..PlaybackOptions::default()
        };
        let frame_count = schedule(&animation, &options).count();
        if frame_count == 0 {
            return Err(format!(
                "{}: converted animation scheduled no frames",
                path.display()
            ));
        }

        let output = destination.join(format!("{slug}.json"));
        let json = serde_json::to_string_pretty(&animation)
            .map_err(|error| format!("{}: could not encode: {error}", path.display()))?;
        fs::write(&output, format!("{json}\n"))
            .map_err(|error| format!("{}: could not write: {error}", output.display()))?;
        println!(
            "OK {} -> {} ({frame_count} frames)",
            path.display(),
            output.display()
        );
        count += 1;
    }
    println!("Converted {count} animations");
    Ok(())
}

fn convert_mode(path: &Path, keyframe: usize, params: SourceParams) -> Result<Mode, String> {
    let detail = || format!("{}: keyframe {keyframe}", path.display());
    match params.mode.as_str() {
        "HSI" => {
            if params.temp.is_some() {
                return Err(format!("{}: HSI has unexpected temp", detail()));
            }
            let hue = required(params.hue, "hue", &detail)?;
            let sat = required(params.sat, "sat", &detail)?;
            let bri = required(params.bri, "bri", &detail)?;
            let hue = if hue == 360 { 0 } else { hue };
            Ok(Mode::Hsi {
                hue: Hue::new(integer::<u16>(hue, "hue", &detail)?)
                    .map_err(|error| format!("{}: {error}", detail()))?,
                sat: Percent::new(integer::<u8>(sat, "sat", &detail)?)
                    .map_err(|error| format!("{}: {error}", detail()))?,
                bri: Percent::new(integer::<u8>(bri, "bri", &detail)?)
                    .map_err(|error| format!("{}: {error}", detail()))?,
            })
        }
        "CCT" => {
            if params.hue.is_some() || params.sat.is_some() {
                return Err(format!("{}: CCT has unexpected hue or sat", detail()));
            }
            let raw_temp = required(params.temp, "temp", &detail)?;
            let bri = required(params.bri, "bri", &detail)?;
            let converted = if raw_temp > 100 {
                (raw_temp + 50) / 100
            } else {
                raw_temp
            };
            let clamped = converted.clamp(25, 100);
            if clamped != converted {
                eprintln!(
                    "{}: keyframe {keyframe}: clamped CCT temp {converted} to {clamped} hundreds of Kelvin",
                    path.display()
                );
            }
            Ok(Mode::Cct {
                temp: Kelvin::new(integer::<u16>(clamped * 100, "temp", &detail)?)
                    .map_err(|error| format!("{}: {error}", detail()))?,
                bri: Percent::new(integer::<u8>(bri, "bri", &detail)?)
                    .map_err(|error| format!("{}: {error}", detail()))?,
            })
        }
        mode => Err(format!("{}: unexpected mode {mode:?}", detail())),
    }
}

fn required<T>(value: Option<T>, field: &str, detail: &impl Fn() -> String) -> Result<T, String> {
    value.ok_or_else(|| format!("{}: missing {field}", detail()))
}

fn integer<T>(value: i64, field: &str, detail: &impl Fn() -> String) -> Result<T, String>
where
    T: TryFrom<i64>,
{
    T::try_from(value).map_err(|_| format!("{}: invalid {field} value {value}", detail()))
}

fn slug(name: &str) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            if separator && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(character.to_ascii_lowercase());
            separator = false;
        } else if character == ' ' || character == '_' || character == '-' {
            separator = true;
        }
    }
    slug
}

fn build_and_sync_ui(debug: bool) -> Result<(), String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let ui = root.join("crates/lumiere-ui");
    let profile = if debug { "debug" } else { "release" };

    let mut command = Command::new("mise");
    command
        .args(["exec", "--", "dx", "build", "--platform", "web"])
        .current_dir(&ui);
    if !debug {
        command.arg("--release");
    }

    println!("Building Lumière UI ({profile}) in {}", ui.display());
    let status = command
        .status()
        .map_err(|error| format!("failed to start Dioxus build: {error}"))?;
    if !status.success() {
        return Err(format!("Dioxus build failed with {status}"));
    }

    let source = root
        .join("target/dx/lumiere-ui")
        .join(profile)
        .join("web/public");
    let destination = root.join("dist/web");
    sync_directory(&source, &destination)
        .map_err(|error| format!("failed to sync UI assets: {error}"))?;
    println!("Synced {} to {}", source.display(), destination.display());
    Ok(())
}

fn sync_directory(source: &Path, destination: &Path) -> io::Result<()> {
    if !source.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("build output {} does not exist", source.display()),
        ));
    }
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(destination)? {
        let entry = entry?;
        if entry.file_name() == ".gitkeep" {
            continue;
        }
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(path)?;
        } else {
            fs::remove_file(path)?;
        }
    }
    copy_directory(source, destination)
}

fn copy_directory(source: &Path, destination: &Path) -> io::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_directory(&source_path, &destination_path)?;
        } else {
            fs::copy(source_path, destination_path)?;
        }
    }
    Ok(())
}
