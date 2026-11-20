//! Versioned per-project view preferences, independent of editing history.

use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Write},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct ViewPreferences {
    pub version: u32,
    pub layout: String,
    pub monitor_height: f32,
    pub inspector_open: bool,
    pub inspector_tab: String,
    pub inspector_section: String,
    pub browser_tab: String,
    pub snapping: bool,
    pub guides: bool,
    pub media_sort: i32,
    pub media_filter: i32,
    pub scope_mode: i32,
    pub monitor_scope: bool,
    pub keyboard_preset: String,
}

impl Default for ViewPreferences {
    fn default() -> Self {
        Self {
            version: 1,
            layout: "Editing".into(),
            monitor_height: 330.0,
            inspector_open: true,
            inspector_tab: "Properties".into(),
            inspector_section: "Clip".into(),
            browser_tab: "Media".into(),
            snapping: true,
            guides: false,
            media_sort: 0,
            media_filter: 0,
            scope_mode: 0,
            monitor_scope: false,
            keyboard_preset: "Premiere".into(),
        }
    }
}

impl ViewPreferences {
    pub fn validate(&self) -> Result<(), io::Error> {
        if self.version != 1
            || !["Editing", "Assembly"].contains(&self.layout.as_str())
            || !["Properties", "History", "Jobs", "Scopes", "Shortcuts"]
                .contains(&self.inspector_tab.as_str())
            || !["Clip", "Source"].contains(&self.inspector_section.as_str())
            || !["Media", "Markers", "Info"].contains(&self.browser_tab.as_str())
            || !self.monitor_height.is_finite()
            || !(220.0..=4096.0).contains(&self.monitor_height)
            || !(0..=2).contains(&self.media_sort)
            || !(0..=3).contains(&self.media_filter)
            || !(0..=2).contains(&self.scope_mode)
            || !["Premiere", "Prism"].contains(&self.keyboard_preset.as_str())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Unsupported or invalid saved workspace",
            ));
        }
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Option<Self>, io::Error> {
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if metadata.len() > 65_536 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Saved workspace is too large",
            ));
        }
        let preferences: Self = serde_json::from_slice(&fs::read(path)?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        preferences.validate()?;
        Ok(Some(preferences))
    }

    pub fn save(&self, path: &Path) -> Result<(), io::Error> {
        self.validate()?;
        let bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let temporary =
            path.with_extension(format!("workspace-{}-{nonce}.tmp", std::process::id()));
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_preferences_round_trip_and_reject_future_versions()
    -> Result<(), Box<dyn std::error::Error>> {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path =
            std::env::temp_dir().join(format!("prism-view-{}-{nonce}.json", std::process::id()));
        assert!(ViewPreferences::load(&path)?.is_none());
        let settings = ViewPreferences {
            keyboard_preset: "Prism".into(),
            inspector_tab: "Shortcuts".into(),
            scope_mode: 2,
            monitor_scope: true,
            layout: "Assembly".into(),
            inspector_open: false,
            media_sort: 2,
            ..Default::default()
        };
        settings.save(&path)?;
        assert_eq!(ViewPreferences::load(&path)?, Some(settings));
        fs::write(&path, br#"{"layout":"Editing"}"#)?;
        let legacy = ViewPreferences::load(&path)?.ok_or("legacy preferences were not loaded")?;
        assert_eq!(legacy.keyboard_preset, "Premiere");
        assert_eq!(legacy.scope_mode, 0);
        assert!(!legacy.monitor_scope);
        assert!(
            ViewPreferences {
                scope_mode: 3,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        let future = br#"{"version":999}"#;
        fs::write(&path, future)?;
        assert!(ViewPreferences::load(&path).is_err());
        assert_eq!(fs::read(&path)?, future);
        fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn invalid_view_values_are_not_persistable() {
        assert!(
            ViewPreferences {
                monitor_height: f32::NAN,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            ViewPreferences {
                browser_tab: "Missing panel".into(),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            ViewPreferences {
                media_sort: 3,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
}
