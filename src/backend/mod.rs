use cosmic::widget;
use std::{
    collections::{BTreeMap, HashMap},
    error::Error,
    fmt,
    path::PathBuf,
    sync::Arc,
    time::Instant,
};

use crate::{AppId, AppInfo, AppstreamCache, GStreamerCodec, Operation};

/// Enum representing the available backend types
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum BackendName {
    /// Mirror OS unified catalog (SQLite DB)
    Catalog,
    // Kept for serialization compat with cached SearchResults that may reference these names
    FlatpakUser,
    FlatpakSystem,
    Nix,
}

impl BackendName {
    /// Returns the string representation of the backend name
    pub fn as_str(&self) -> &'static str {
        match self {
            BackendName::Catalog => "catalog",
            BackendName::FlatpakUser => "flatpak-user",
            BackendName::FlatpakSystem => "flatpak-system",
            BackendName::Nix => "nix",
        }
    }

    /// Check if this is a flatpak backend
    pub fn is_flatpak(&self) -> bool {
        matches!(self, BackendName::FlatpakUser | BackendName::FlatpakSystem)
    }
}

impl fmt::Display for BackendName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::str::FromStr for BackendName {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "catalog" => Ok(BackendName::Catalog),
            "flatpak-user" => Ok(BackendName::FlatpakUser),
            "flatpak-system" => Ok(BackendName::FlatpakSystem),
            "nix" => Ok(BackendName::Nix),
            _ => Err(format!("unknown backend name: {}", s)),
        }
    }
}

pub mod catalog;
#[cfg(feature = "flatpak")]
mod flatpak;
pub mod hm_options;
pub mod nix;

#[derive(Clone, Debug)]
pub struct Package {
    pub id: AppId,
    pub icon: widget::icon::Handle,
    pub info: Arc<AppInfo>,
    pub version: String,
    pub extra: HashMap<String, String>,
}

pub trait Backend: fmt::Debug + Send + Sync {
    fn load_caches(&mut self, refresh: bool) -> Result<(), Box<dyn Error>>;
    fn info_caches(&self) -> &[AppstreamCache];

    /// Return the backend's full app info map, if it maintains one directly
    /// (used by catalog backend instead of AppstreamCache).
    fn catalog_infos(&self) -> Option<&HashMap<AppId, Arc<AppInfo>>> {
        None
    }

    /// Resolve an icon handle for the given AppInfo.
    /// Backends that manage local media caches override this.
    fn resolve_icon(&self, _info: &AppInfo) -> Option<widget::icon::Handle> {
        None
    }

    fn installed(&self) -> Result<Vec<Package>, Box<dyn Error>>;
    fn updates(&self) -> Result<Vec<Package>, Box<dyn Error>>;
    fn file_packages(&self, path: &str) -> Result<Vec<Package>, Box<dyn Error>>;
    fn gstreamer_packages(
        &self,
        _gstreamer_codec: &GStreamerCodec,
    ) -> Result<Vec<Package>, Box<dyn Error>> {
        Ok(Vec::new())
    }
    fn operation(
        &self,
        op: &Operation,
        f: Box<dyn FnMut(f32) + 'static>,
    ) -> Result<(), Box<dyn Error>>;
}

// BTreeMap for stable sort order
pub type Backends = BTreeMap<BackendName, Arc<dyn Backend>>;

pub fn backends(_locale: &str, refresh: bool) -> Backends {
    let mut backends = Backends::new();

    let db_path = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("mirror-os/catalog.db");

    let media_dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("mirror-os/media");

    let start = Instant::now();
    let mut backend = catalog::CatalogBackend::new(db_path.clone(), media_dir);
    match backend.load_caches(refresh) {
        Ok(()) => {
            log::info!("catalog backend loaded in {:?}", start.elapsed());
        }
        Err(err) => {
            log::error!("catalog backend load failed: {} (db: {})", err, db_path.display());
        }
    }
    backends.insert(BackendName::Catalog, Arc::new(backend));

    backends
}
