use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use directories::ProjectDirs;
use lumiere_proto::{
    Hue, Kelvin, LightId, Mode, Percent, Preset, PresetEntry, PresetId, PresetTarget,
};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use thiserror::Error;
use tokio::{sync::mpsc, task::JoinHandle};

const STORE_CHANNEL_CAPACITY: usize = 64;
const STORE_DEBOUNCE: Duration = Duration::from_millis(500);

/// The complete persistent state loaded for the registry.
#[derive(Clone, Debug)]
pub struct StoreData {
    pub labels: HashMap<LightId, String>,
    pub presets: Vec<Preset>,
}

/// A persisted registry update.
#[derive(Clone, Debug)]
pub enum StoreUpdate {
    Label { id: LightId, label: String },
    Presets(Vec<Preset>),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredLight {
    label: String,
}

const LIGHTS_FILE: &str = "lights.toml";
const PRESETS_FILE: &str = "presets.toml";

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredPresets {
    presets: Vec<Preset>,
}

/// Returns the configured data directory holding all store files.
pub fn default_dir() -> Result<PathBuf, StoreError> {
    if let Some(path) = std::env::var_os("LUMIERE_DATA_DIR") {
        Ok(PathBuf::from(path))
    } else {
        ProjectDirs::from("", "", "lumiere")
            .map(|dirs| dirs.data_dir().to_owned())
            .ok_or(StoreError::NoDataDirectory)
    }
}

/// Loads saved labels and ordered presets from an injectable data directory.
pub fn load(dir: &Path) -> Result<StoreData, StoreError> {
    let labels = match fs::read_to_string(dir.join(LIGHTS_FILE)) {
        Ok(encoded) => {
            let stored: BTreeMap<String, StoredLight> = toml::from_str(&encoded)?;
            stored
                .into_iter()
                .map(|(id, light)| Ok((LightId::parse(&id)?, light.label)))
                .collect::<Result<_, StoreError>>()?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
        Err(error) => return Err(error.into()),
    };
    let presets = match fs::read_to_string(dir.join(PRESETS_FILE)) {
        Ok(encoded) => toml::from_str::<StoredPresets>(&encoded)?.presets,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => factory_presets(),
        Err(error) => return Err(error.into()),
    };
    Ok(StoreData { labels, presets })
}

/// Starts the debounced persistence task for an injectable data directory.
pub fn spawn(
    dir: PathBuf,
    data: StoreData,
) -> (
    mpsc::Sender<StoreUpdate>,
    JoinHandle<Result<(), StoreError>>,
) {
    let (tx, rx) = mpsc::channel(STORE_CHANNEL_CAPACITY);
    let task = tokio::spawn(run(dir, data, rx));
    (tx, task)
}

async fn run(
    dir: PathBuf,
    mut data: StoreData,
    mut rx: mpsc::Receiver<StoreUpdate>,
) -> Result<(), StoreError> {
    let presets_path = dir.join(PRESETS_FILE);
    if !presets_path.exists() {
        write_presets(&presets_path, &data.presets)?;
    }
    while let Some(update) = rx.recv().await {
        let (mut labels_dirty, mut presets_dirty) = apply_update(&mut data, update);
        loop {
            tokio::select! {
                next = rx.recv() => match next {
                    Some(update) => {
                        let dirty = apply_update(&mut data, update);
                        labels_dirty |= dirty.0;
                        presets_dirty |= dirty.1;
                    }
                    None => {
                        write_dirty(&dir.join(LIGHTS_FILE), &presets_path, &data, labels_dirty, presets_dirty)?;
                        return Ok(());
                    }
                },
                () = tokio::time::sleep(STORE_DEBOUNCE) => break,
            }
        }
        write_dirty(
            &dir.join(LIGHTS_FILE),
            &presets_path,
            &data,
            labels_dirty,
            presets_dirty,
        )?;
    }
    Ok(())
}

fn apply_update(data: &mut StoreData, update: StoreUpdate) -> (bool, bool) {
    match update {
        StoreUpdate::Label { id, label } => {
            data.labels.insert(id, label);
            (true, false)
        }
        StoreUpdate::Presets(presets) => {
            data.presets = presets;
            (false, true)
        }
    }
}

fn write_dirty(
    path: &Path,
    presets_path: &Path,
    data: &StoreData,
    labels_dirty: bool,
    presets_dirty: bool,
) -> Result<(), StoreError> {
    if labels_dirty {
        write_labels(path, &data.labels)?;
    }
    if presets_dirty {
        write_presets(presets_path, &data.presets)?;
    }
    Ok(())
}

fn write_labels(path: &Path, labels: &HashMap<LightId, String>) -> Result<(), StoreError> {
    let stored: BTreeMap<_, _> = labels
        .iter()
        .map(|(id, label)| {
            (
                id.to_string(),
                StoredLight {
                    label: label.clone(),
                },
            )
        })
        .collect();
    atomic_write(path, &toml::to_string_pretty(&stored)?)
}

fn write_presets(path: &Path, presets: &[Preset]) -> Result<(), StoreError> {
    atomic_write(
        path,
        &toml::to_string_pretty(&StoredPresets {
            presets: presets.to_vec(),
        })?,
    )
}

fn atomic_write(path: &Path, encoded: &str) -> Result<(), StoreError> {
    let parent = path.parent().ok_or(StoreError::InvalidPath)?;
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(encoded.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// Returns the eight presets installed when no preset store exists.
pub fn factory_presets() -> Vec<Preset> {
    let cct = |id: &str, name: &str, temp: u16, bri: u8| Preset {
        id: PresetId::parse(id).expect("factory preset id is valid"),
        name: name.to_owned(),
        entries: vec![PresetEntry {
            target: PresetTarget::Everything,
            mode: Mode::Cct {
                temp: Kelvin::new(temp).expect("factory kelvin is valid"),
                bri: Percent::new(bri).expect("factory brightness is valid"),
            },
        }],
    };
    let hsi = |id: &str, name: &str, hue: u16| Preset {
        id: PresetId::parse(id).expect("factory preset id is valid"),
        name: name.to_owned(),
        entries: vec![PresetEntry {
            target: PresetTarget::Everything,
            mode: Mode::Hsi {
                hue: Hue::new(hue).expect("factory hue is valid"),
                sat: Percent::new(100).expect("factory saturation is valid"),
                bri: Percent::new(20).expect("factory brightness is valid"),
            },
        }],
    };
    vec![
        cct("daylight", "Daylight", 5600, 20),
        cct("warm", "Warm", 3200, 20),
        cct("blackout", "Blackout", 5600, 0),
        hsi("red", "Red", 0),
        hsi("blue", "Blue", 240),
        hsi("green", "Green", 120),
        hsi("purple", "Purple", 300),
        hsi("cyan", "Cyan", 160),
    ]
}

/// Failure to load or persist registry state.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("could not determine the data directory")]
    NoDataDirectory,
    #[error("store path has no parent directory")]
    InvalidPath,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Decode(#[from] toml::de::Error),
    #[error(transparent)]
    Encode(#[from] toml::ser::Error),
    #[error(transparent)]
    InvalidLightId(#[from] lumiere_proto::IdError),
}
