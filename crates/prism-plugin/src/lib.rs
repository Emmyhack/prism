//! Versioned plugin metadata and capability validation.
//!
//! This crate intentionally stops at a declarative manifest boundary. Runtime
//! loading, Wasmtime limits, and trusted native extensions are separate
//! components and must not be implied by a manifest parsing success.

use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};
use thiserror::Error;

/// The manifest schema version understood by this build.
pub const MANIFEST_API_VERSION: u32 = 1;

/// Plugin categories exposed in the extension manifest.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginKind {
    /// A video processing effect.
    VideoEffect,
    /// An audio processing effect.
    AudioEffect,
    /// A timeline transition.
    Transition,
    /// A media importer.
    Importer,
    /// An export backend.
    Exporter,
    /// A script API extension.
    ScriptExtension,
    /// A trusted or sandboxed UI panel.
    UiPanel,
    /// A metadata analyzer.
    MetadataAnalyzer,
}

/// Capabilities a plugin must explicitly declare before activation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    /// Read project state.
    ProjectRead,
    /// Mutate project state through validated commands.
    ProjectWrite,
    /// Read media metadata or decoded content.
    MediaRead,
    /// Write rendered/exported media.
    ExportCreate,
    /// Invoke a registered script extension.
    ScriptInvoke,
    /// Read files through a host-mediated capability.
    FilesystemRead,
    /// Write files through a host-mediated capability.
    FilesystemWrite,
    /// Access network resources through a host-mediated capability.
    NetworkAccess,
}

/// Declarative plugin manifest exchanged before any runtime is started.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PluginManifest {
    /// Stable reverse-DNS-style plugin identifier.
    pub id: String,
    /// Plugin release version, retained as an opaque non-empty string.
    pub version: String,
    /// Manifest schema version.
    pub api_version: u32,
    /// Functional plugin category.
    pub kind: PluginKind,
    /// Runtime entry module or component name.
    pub entry: String,
    /// Capabilities requested by the plugin.
    pub capabilities: Vec<Capability>,
}

/// Manifest validation failures.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum ManifestError {
    /// The identifier is empty or contains unsupported characters.
    #[error("plugin id must use non-empty reverse-DNS-style characters")]
    InvalidId,
    /// A release version is required.
    #[error("plugin version must not be empty")]
    EmptyVersion,
    /// The manifest schema is newer than this application understands.
    #[error("unsupported plugin manifest API version {found}")]
    UnsupportedApiVersion {
        /// Version supplied by the manifest.
        found: u32,
    },
    /// An executable entry is required.
    #[error("plugin entry must not be empty")]
    EmptyEntry,
    /// A capability may only be declared once.
    #[error("plugin capability is declared more than once")]
    DuplicateCapability,
    /// A requested capability was not granted by the host.
    #[error("plugin capability was not granted: {capability:?}")]
    MissingCapability {
        /// Capability requested by the manifest.
        capability: Capability,
    },
}

/// A validated plugin manifest and its resolved local entry path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginDescriptor {
    /// Validated metadata loaded from `plugin.json`.
    pub manifest: PluginManifest,
    /// Manifest file on disk.
    pub manifest_path: PathBuf,
    /// Entry component/module on disk, constrained to the plugin directory.
    pub entry_path: PathBuf,
}

/// Plugin catalog discovery failures.
#[derive(Debug, Error)]
pub enum CatalogError {
    /// Filesystem traversal or reads failed.
    #[error("plugin catalog I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// A manifest did not satisfy the plugin contract.
    #[error("invalid plugin manifest {path}: {source}")]
    InvalidManifest {
        /// Manifest that failed validation.
        path: PathBuf,
        /// Validation failure.
        source: ManifestError,
    },
    /// Manifest JSON could not be decoded.
    #[error("could not parse plugin manifest {path}: {source}")]
    ManifestParse {
        /// Manifest that failed decoding.
        path: PathBuf,
        /// JSON parser failure.
        source: serde_json::Error,
    },
    /// Entry paths must stay inside their plugin directory.
    #[error("plugin entry path is unsafe: {0}")]
    UnsafeEntry(PathBuf),
    /// A declared entry file does not exist or is not regular.
    #[error("plugin entry is not a regular file: {0}")]
    MissingEntry(PathBuf),
    /// Two discovered manifests declared the same stable identifier.
    #[error("duplicate plugin id: {0}")]
    DuplicateId(String),
}

/// Discovers and validates local plugin packages without activating runtimes.
pub struct PluginCatalog;

impl PluginCatalog {
    /// Loads immediate child plugin directories in deterministic path order.
    ///
    /// Each child must contain `plugin.json`; its manifest `entry` is resolved
    /// relative to that child and must point to a regular file. No code is
    /// loaded or executed by discovery.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError`] for filesystem, manifest, path-safety, missing
    /// entry, or duplicate-identifier failures.
    pub fn discover(root: &Path) -> Result<Vec<PluginDescriptor>, CatalogError> {
        let root = root.canonicalize()?;
        let mut directories = fs::read_dir(&root)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()?;
        directories.sort();
        let mut descriptors = Vec::new();
        for directory in directories.into_iter().filter(|path| path.is_dir()) {
            let manifest_path = directory.join("plugin.json");
            if !manifest_path.is_file() {
                continue;
            }
            let bytes = fs::read(&manifest_path)?;
            let manifest: PluginManifest =
                serde_json::from_slice(&bytes).map_err(|source| CatalogError::ManifestParse {
                    path: manifest_path.clone(),
                    source,
                })?;
            manifest
                .validate()
                .map_err(|source| CatalogError::InvalidManifest {
                    path: manifest_path.clone(),
                    source,
                })?;
            if descriptors
                .iter()
                .any(|descriptor: &PluginDescriptor| descriptor.manifest.id == manifest.id)
            {
                return Err(CatalogError::DuplicateId(manifest.id));
            }
            let entry = Path::new(&manifest.entry);
            if entry.is_absolute()
                || entry.components().any(|component| {
                    matches!(
                        component,
                        Component::ParentDir | Component::RootDir | Component::Prefix(_)
                    )
                })
            {
                return Err(CatalogError::UnsafeEntry(entry.to_path_buf()));
            }
            let entry_path = directory
                .join(entry)
                .canonicalize()
                .map_err(|_| CatalogError::MissingEntry(directory.join(entry)))?;
            if !entry_path.starts_with(&directory) || !entry_path.is_file() {
                return Err(CatalogError::UnsafeEntry(entry_path));
            }
            descriptors.push(PluginDescriptor {
                manifest,
                manifest_path,
                entry_path,
            });
        }
        Ok(descriptors)
    }
}

impl PluginManifest {
    /// Validates all fields required before a plugin can be considered.
    ///
    /// # Errors
    ///
    /// Returns a [`ManifestError`] when identity, version, entry, API version,
    /// or capability declarations are invalid.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.id.is_empty()
            || self.id.starts_with('.')
            || self.id.ends_with('.')
            || self.id.split('.').any(|part| {
                part.is_empty()
                    || !part
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric() || character == '-')
            })
        {
            return Err(ManifestError::InvalidId);
        }
        if self.version.trim().is_empty() {
            return Err(ManifestError::EmptyVersion);
        }
        if self.api_version != MANIFEST_API_VERSION {
            return Err(ManifestError::UnsupportedApiVersion {
                found: self.api_version,
            });
        }
        if self.entry.trim().is_empty() {
            return Err(ManifestError::EmptyEntry);
        }
        for (index, capability) in self.capabilities.iter().enumerate() {
            if self.capabilities[..index].contains(capability) {
                return Err(ManifestError::DuplicateCapability);
            }
        }
        Ok(())
    }

    /// Validates that every requested capability is present in the host grant.
    ///
    /// Manifest metadata is validated first. A host should call this method
    /// immediately before activation and never infer authorization from the
    /// plugin kind alone.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::MissingCapability`] for the first requested
    /// capability absent from `granted`.
    pub fn authorize(&self, granted: &[Capability]) -> Result<(), ManifestError> {
        self.validate()?;
        for capability in &self.capabilities {
            if !granted.contains(capability) {
                return Err(ManifestError::MissingCapability {
                    capability: *capability,
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Capability, CatalogError, MANIFEST_API_VERSION, ManifestError, PluginCatalog, PluginKind,
        PluginManifest,
    };
    use std::fs;

    fn manifest() -> PluginManifest {
        PluginManifest {
            id: "com.example.wave-shaper".to_owned(),
            version: "1.0.0".to_owned(),
            api_version: MANIFEST_API_VERSION,
            kind: PluginKind::AudioEffect,
            entry: "component.wasm".to_owned(),
            capabilities: vec![Capability::MediaRead, Capability::ProjectRead],
        }
    }

    #[test]
    fn valid_manifest_round_trips_as_stable_json() -> Result<(), Box<dyn std::error::Error>> {
        let original = manifest();
        original.validate()?;
        let json = serde_json::to_string(&original)?;
        assert!(json.contains("audio-effect"));
        assert!(json.contains("media-read"));
        assert_eq!(serde_json::from_str::<PluginManifest>(&json)?, original);
        Ok(())
    }

    #[test]
    fn validation_rejects_unsafe_or_ambiguous_manifests() {
        let mut invalid = manifest();
        invalid.id = "../escape".to_owned();
        assert_eq!(invalid.validate(), Err(ManifestError::InvalidId));

        let mut duplicate = manifest();
        duplicate.capabilities.push(Capability::MediaRead);
        assert_eq!(
            duplicate.validate(),
            Err(ManifestError::DuplicateCapability)
        );

        let mut future = manifest();
        future.api_version = MANIFEST_API_VERSION + 1;
        assert_eq!(
            future.validate(),
            Err(ManifestError::UnsupportedApiVersion {
                found: MANIFEST_API_VERSION + 1
            })
        );
        assert_eq!(
            manifest().authorize(&[Capability::ProjectRead]),
            Err(ManifestError::MissingCapability {
                capability: Capability::MediaRead
            })
        );
        assert!(
            manifest()
                .authorize(&[Capability::ProjectRead, Capability::MediaRead])
                .is_ok()
        );
        let mut external = manifest();
        external.capabilities = vec![Capability::FilesystemRead, Capability::NetworkAccess];
        assert_eq!(
            external.authorize(&[Capability::FilesystemRead]),
            Err(ManifestError::MissingCapability {
                capability: Capability::NetworkAccess
            })
        );
        assert!(
            external
                .authorize(&[Capability::FilesystemRead, Capability::NetworkAccess])
                .is_ok()
        );
    }

    #[test]
    fn catalog_discovers_sorted_valid_plugins_and_rejects_duplicate_ids()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "prism-plugin-catalog-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        fs::create_dir(&root)?;
        for name in ["z-plugin", "a-plugin"] {
            let directory = root.join(name);
            fs::create_dir(&directory)?;
            fs::write(directory.join("component.wasm"), b"wasm")?;
            let mut value = manifest();
            value.id = format!("com.example.{name}");
            fs::write(directory.join("plugin.json"), serde_json::to_vec(&value)?)?;
        }
        let descriptors = PluginCatalog::discover(&root)?;
        assert_eq!(descriptors.len(), 2);
        assert_eq!(descriptors[0].manifest.id, "com.example.a-plugin");
        assert!(descriptors[0].entry_path.is_file());
        let duplicate = root.join("duplicate");
        fs::create_dir(&duplicate)?;
        fs::write(duplicate.join("component.wasm"), b"wasm")?;
        let mut duplicate_manifest = manifest();
        duplicate_manifest.id = "com.example.a-plugin".to_owned();
        fs::write(
            duplicate.join("plugin.json"),
            serde_json::to_vec(&duplicate_manifest)?,
        )?;
        assert!(matches!(
            PluginCatalog::discover(&root),
            Err(CatalogError::DuplicateId(_))
        ));
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn catalog_rejects_traversal_and_missing_entries() -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "prism-plugin-paths-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        fs::create_dir(&root)?;
        let directory = root.join("unsafe");
        fs::create_dir(&directory)?;
        let mut traversal = manifest();
        traversal.entry = "../outside.wasm".to_owned();
        fs::write(
            directory.join("plugin.json"),
            serde_json::to_vec(&traversal)?,
        )?;
        assert!(matches!(
            PluginCatalog::discover(&root),
            Err(CatalogError::UnsafeEntry(_))
        ));
        fs::remove_file(directory.join("plugin.json"))?;
        let missing = manifest();
        fs::write(directory.join("plugin.json"), serde_json::to_vec(&missing)?)?;
        assert!(matches!(
            PluginCatalog::discover(&root),
            Err(CatalogError::MissingEntry(_))
        ));
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
