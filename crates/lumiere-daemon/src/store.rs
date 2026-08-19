use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use directories::ProjectDirs;
use lumiere_proto::LightId;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use thiserror::Error;
use tokio::{sync::mpsc, task::JoinHandle};

const STORE_CHANNEL_CAPACITY: usize = 64;
const STORE_DEBOUNCE: Duration = Duration::from_millis(500);

/// A persisted light-field update.
#[derive(Clone, Debug)]
pub struct StoreUpdate {
    pub id: LightId,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredLight {
    label: String,
}

/// Returns the configured label-store path.
pub fn default_path() -> Result<PathBuf, StoreError> {
    let directory = if let Some(path) = std::env::var_os("LUMIERE_DATA_DIR") {
        PathBuf::from(path)
    } else {
        ProjectDirs::from("", "", "lumiere")
            .map(|dirs| dirs.data_dir().to_owned())
            .ok_or(StoreError::NoDataDirectory)?
    };
    Ok(directory.join("lights.toml"))
}

/// Loads saved labels from an injectable path.
pub fn load(path: &Path) -> Result<HashMap<LightId, String>, StoreError> {
    let encoded = match fs::read_to_string(path) {
        Ok(encoded) => encoded,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(error) => return Err(error.into()),
    };
    let stored: BTreeMap<String, StoredLight> = toml::from_str(&encoded)?;
    stored
        .into_iter()
        .map(|(id, light)| Ok((LightId::parse(&id)?, light.label)))
        .collect()
}

/// Starts the debounced persistence task for an injectable path.
pub fn spawn(
    path: PathBuf,
    labels: HashMap<LightId, String>,
) -> (
    mpsc::Sender<StoreUpdate>,
    JoinHandle<Result<(), StoreError>>,
) {
    let (tx, rx) = mpsc::channel(STORE_CHANNEL_CAPACITY);
    let task = tokio::spawn(run(path, labels, rx));
    (tx, task)
}

async fn run(
    path: PathBuf,
    mut labels: HashMap<LightId, String>,
    mut rx: mpsc::Receiver<StoreUpdate>,
) -> Result<(), StoreError> {
    while let Some(update) = rx.recv().await {
        labels.insert(update.id, update.label);
        loop {
            tokio::select! {
                next = rx.recv() => match next {
                    Some(update) => {
                        labels.insert(update.id, update.label);
                    }
                    None => {
                        write_store(&path, &labels)?;
                        return Ok(());
                    }
                },
                () = tokio::time::sleep(STORE_DEBOUNCE) => break,
            }
        }
        write_store(&path, &labels)?;
    }
    Ok(())
}

fn write_store(path: &Path, labels: &HashMap<LightId, String>) -> Result<(), StoreError> {
    let parent = path.parent().ok_or(StoreError::InvalidPath)?;
    fs::create_dir_all(parent)?;
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
    let encoded = toml::to_string_pretty(&stored)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(encoded.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// Failure to load or persist the light store.
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
