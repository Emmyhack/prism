//! Versioned recipe manifests and parameter validation.
//!
//! Recipes are declarative entry points over Prism Script. This crate validates
//! their metadata and user inputs before a script worker is started; it does
//! not execute scripts or grant capabilities.

use std::collections::BTreeMap;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

/// The recipe manifest schema version understood by this build.
pub const RECIPE_API_VERSION: u32 = 1;

/// Supported recipe authoring languages.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RecipeLanguage {
    /// Python recipe source.
    Python,
    /// JavaScript recipe source.
    JavaScript,
    /// TypeScript recipe source compiled before execution.
    TypeScript,
}

/// Capabilities a recipe must request before execution.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RecipePermission {
    /// Read project state.
    #[serde(rename = "project.read")]
    ProjectRead,
    /// Mutate project state through commands.
    #[serde(rename = "project.write")]
    ProjectWrite,
    /// Read media metadata or decoded content.
    #[serde(rename = "media.read")]
    MediaRead,
    /// Import source media.
    #[serde(rename = "media.import")]
    MediaImport,
    /// Create an export.
    #[serde(rename = "export.create")]
    ExportCreate,
    /// Read files outside the project database.
    #[serde(rename = "filesystem.read")]
    FilesystemRead,
    /// Write files outside the project database.
    #[serde(rename = "filesystem.write")]
    FilesystemWrite,
    /// Access network resources.
    #[serde(rename = "network.access")]
    NetworkAccess,
    /// Invoke an explicitly trusted plugin.
    #[serde(rename = "plugin.invoke")]
    PluginInvoke,
}

/// Parameter value types accepted by a recipe.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ParameterType {
    /// A signed whole number.
    Integer,
    /// A finite JSON number.
    Number,
    /// A text value.
    String,
    /// A true/false value.
    Boolean,
}

/// Schema for one recipe parameter.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ParameterSchema {
    /// Expected JSON value type.
    #[serde(rename = "type")]
    pub value_type: ParameterType,
    /// Inclusive minimum for numeric values.
    pub minimum: Option<f64>,
    /// Inclusive maximum for numeric values.
    pub maximum: Option<f64>,
    /// Optional default; absence makes the parameter required.
    pub default: Option<Value>,
}

/// Declarative recipe manifest.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipeManifest {
    /// Stable reverse-DNS-style recipe identifier.
    pub id: String,
    /// Recipe release version.
    pub version: String,
    /// Manifest schema version.
    pub api_version: u32,
    /// Source language for the recipe entry.
    pub language: RecipeLanguage,
    /// Script entry module.
    pub entry: String,
    /// Capabilities requested before execution.
    pub permissions: Vec<RecipePermission>,
    /// Named user-configurable parameters.
    pub parameters: BTreeMap<String, ParameterSchema>,
}

impl RecipeManifest {
    /// Returns the number of declared recipe parameters.
    #[must_use]
    pub fn parameter_count(&self) -> usize {
        self.parameters.len()
    }

    /// Returns the number of parameters without defaults.
    #[must_use]
    pub fn required_parameter_count(&self) -> usize {
        self.parameters
            .values()
            .filter(|parameter| parameter.default.is_none())
            .count()
    }
}

/// Recipe validation failures.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum RecipeError {
    /// The recipe identifier is empty or contains unsupported characters.
    #[error("recipe id must use non-empty reverse-DNS-style characters")]
    InvalidId,
    /// A release version is required.
    #[error("recipe version must not be empty")]
    EmptyVersion,
    /// The manifest schema is newer than this application understands.
    #[error("unsupported recipe API version {found}")]
    UnsupportedApiVersion {
        /// Version supplied by the manifest.
        found: u32,
    },
    /// A script entry is required.
    #[error("recipe entry must not be empty")]
    EmptyEntry,
    /// The script entry must remain inside the recipe directory.
    #[error("recipe entry must be a relative path contained by the recipe directory")]
    UnsafeEntry,
    /// Numeric bounds are malformed.
    #[error("invalid numeric bounds for parameter {name}")]
    InvalidBounds {
        /// Parameter with malformed numeric bounds.
        name: String,
    },
    /// A default does not satisfy its declared schema.
    #[error("invalid default for parameter {name}")]
    InvalidDefault {
        /// Parameter whose default has the wrong type or range.
        name: String,
    },
    /// An input was not declared by the manifest.
    #[error("unknown recipe parameter {name}")]
    UnknownParameter {
        /// Input name not present in the manifest.
        name: String,
    },
    /// A required input was omitted.
    #[error("missing required recipe parameter {name}")]
    MissingParameter {
        /// Required parameter that was omitted.
        name: String,
    },
    /// An input has the wrong JSON type.
    #[error("invalid value for recipe parameter {name}")]
    InvalidParameter {
        /// Input whose value has the wrong type.
        name: String,
    },
    /// An input falls outside numeric bounds.
    #[error("recipe parameter {name} is outside its allowed range")]
    OutOfRange {
        /// Input outside the declared numeric range.
        name: String,
    },
    /// A requested permission was not granted by the host.
    #[error("recipe permission was not granted: {permission:?}")]
    MissingPermission {
        /// Permission requested by the recipe.
        permission: RecipePermission,
    },
}

impl RecipeManifest {
    /// Validates manifest metadata and all parameter schemas.
    ///
    /// # Errors
    ///
    /// Returns [`RecipeError`] for malformed identity, version, bounds, or
    /// defaults.
    pub fn validate(&self) -> Result<(), RecipeError> {
        if !valid_identifier(&self.id) {
            return Err(RecipeError::InvalidId);
        }
        if self.version.trim().is_empty() {
            return Err(RecipeError::EmptyVersion);
        }
        if self.api_version != RECIPE_API_VERSION {
            return Err(RecipeError::UnsupportedApiVersion {
                found: self.api_version,
            });
        }
        if self.entry.trim().is_empty() {
            return Err(RecipeError::EmptyEntry);
        }
        let entry = Path::new(&self.entry);
        if entry.is_absolute()
            || entry.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(RecipeError::UnsafeEntry);
        }
        for (name, schema) in &self.parameters {
            if !schema_bounds_valid(schema) {
                return Err(RecipeError::InvalidBounds { name: name.clone() });
            }
            if let Some(default) = &schema.default {
                validate_value(name, schema, default, true)?;
            }
        }
        Ok(())
    }

    /// Validates user inputs against this manifest after metadata validation.
    ///
    /// # Errors
    ///
    /// Returns [`RecipeError`] for unknown, missing, incorrectly typed, or
    /// out-of-range inputs.
    pub fn validate_inputs(&self, inputs: &Map<String, Value>) -> Result<(), RecipeError> {
        self.validate()?;
        for name in inputs.keys() {
            if !self.parameters.contains_key(name) {
                return Err(RecipeError::UnknownParameter { name: name.clone() });
            }
        }
        for (name, schema) in &self.parameters {
            match inputs.get(name) {
                Some(value) => validate_value(name, schema, value, false)?,
                None if schema.default.is_some() => {}
                None => return Err(RecipeError::MissingParameter { name: name.clone() }),
            }
        }
        Ok(())
    }

    /// Validates that every requested permission is present in the host grant.
    ///
    /// # Errors
    ///
    /// Returns [`RecipeError::MissingPermission`] for the first permission
    /// absent from `granted`.
    pub fn authorize(&self, granted: &[RecipePermission]) -> Result<(), RecipeError> {
        self.validate()?;
        for permission in &self.permissions {
            if !granted.contains(permission) {
                return Err(RecipeError::MissingPermission {
                    permission: *permission,
                });
            }
        }
        Ok(())
    }
}

fn valid_identifier(identifier: &str) -> bool {
    !identifier.is_empty()
        && !identifier.starts_with('.')
        && !identifier.ends_with('.')
        && identifier.split('.').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '-')
        })
}

fn schema_bounds_valid(schema: &ParameterSchema) -> bool {
    schema.minimum.is_none_or(f64::is_finite)
        && schema.maximum.is_none_or(f64::is_finite)
        && match (schema.minimum, schema.maximum) {
            (Some(minimum), Some(maximum)) => minimum <= maximum,
            _ => true,
        }
}

fn validate_value(
    name: &str,
    schema: &ParameterSchema,
    value: &Value,
    is_default: bool,
) -> Result<(), RecipeError> {
    let number = match schema.value_type {
        ParameterType::Integer if value.as_i64().is_some() || value.as_u64().is_some() => {
            value.as_f64()
        }
        ParameterType::Number => value.as_f64(),
        ParameterType::String if value.is_string() => None,
        ParameterType::Boolean if value.is_boolean() => None,
        _ => {
            return Err(if is_default {
                RecipeError::InvalidDefault {
                    name: name.to_owned(),
                }
            } else {
                RecipeError::InvalidParameter {
                    name: name.to_owned(),
                }
            });
        }
    };
    if let Some(number) = number
        && (!number.is_finite()
            || schema.minimum.is_some_and(|minimum| number < minimum)
            || schema.maximum.is_some_and(|maximum| number > maximum))
    {
        return Err(RecipeError::OutOfRange {
            name: name.to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::{Map, Value, json};

    use super::{
        ParameterSchema, ParameterType, RECIPE_API_VERSION, RecipeError, RecipeLanguage,
        RecipeManifest, RecipePermission,
    };

    fn manifest() -> RecipeManifest {
        let mut parameters = BTreeMap::new();
        parameters.insert(
            "clipCount".to_owned(),
            ParameterSchema {
                value_type: ParameterType::Integer,
                minimum: Some(1.0),
                maximum: Some(20.0),
                default: Some(json!(5)),
            },
        );
        parameters.insert(
            "maxDuration".to_owned(),
            ParameterSchema {
                value_type: ParameterType::Number,
                minimum: Some(0.1),
                maximum: None,
                default: Some(json!(30.0)),
            },
        );
        RecipeManifest {
            id: "prism.podcast-to-shorts".to_owned(),
            version: "1.0.0".to_owned(),
            api_version: RECIPE_API_VERSION,
            language: RecipeLanguage::TypeScript,
            entry: "main.ts".to_owned(),
            permissions: vec![
                RecipePermission::ProjectRead,
                RecipePermission::ProjectWrite,
            ],
            parameters,
        }
    }

    #[test]
    fn manifest_round_trips_and_validates_inputs() -> Result<(), Box<dyn std::error::Error>> {
        let original = manifest();
        original.validate()?;
        assert!(
            original
                .authorize(&[
                    RecipePermission::ProjectRead,
                    RecipePermission::ProjectWrite
                ])
                .is_ok()
        );
        let encoded = serde_json::to_string(&original)?;
        assert!(encoded.contains("project.read"));
        assert_eq!(serde_json::from_str::<RecipeManifest>(&encoded)?, original);

        let mut inputs = Map::new();
        inputs.insert("clipCount".to_owned(), json!(10));
        inputs.insert("maxDuration".to_owned(), json!(45.0));
        original.validate_inputs(&inputs)?;
        Ok(())
    }

    #[test]
    fn input_validation_rejects_missing_unknown_wrong_and_out_of_range_values() {
        let mut required = manifest();
        if let Some(schema) = required.parameters.get_mut("maxDuration") {
            schema.default = None;
        }
        let empty = Map::new();
        assert_eq!(
            required.validate_inputs(&empty),
            Err(RecipeError::MissingParameter {
                name: "maxDuration".to_owned()
            })
        );

        let mut unknown = Map::new();
        unknown.insert("other".to_owned(), json!(true));
        assert_eq!(
            manifest().validate_inputs(&unknown),
            Err(RecipeError::UnknownParameter {
                name: "other".to_owned()
            })
        );

        let mut wrong = Map::new();
        wrong.insert("clipCount".to_owned(), Value::String("ten".to_owned()));
        assert_eq!(
            manifest().validate_inputs(&wrong),
            Err(RecipeError::InvalidParameter {
                name: "clipCount".to_owned()
            })
        );

        let mut out_of_range = Map::new();
        out_of_range.insert("clipCount".to_owned(), json!(21));
        assert_eq!(
            manifest().validate_inputs(&out_of_range),
            Err(RecipeError::OutOfRange {
                name: "clipCount".to_owned()
            })
        );
        assert_eq!(
            manifest().authorize(&[RecipePermission::ProjectRead]),
            Err(RecipeError::MissingPermission {
                permission: RecipePermission::ProjectWrite
            })
        );
    }

    #[test]
    fn manifest_rejects_entries_outside_the_recipe_directory() {
        let mut recipe = manifest();
        recipe.entry = "../outside.ts".to_owned();
        assert_eq!(recipe.validate(), Err(RecipeError::UnsafeEntry));

        recipe.entry = "/absolute/main.ts".to_owned();
        assert_eq!(recipe.validate(), Err(RecipeError::UnsafeEntry));
    }
}
