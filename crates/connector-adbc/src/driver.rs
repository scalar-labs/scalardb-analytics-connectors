//! The ADBC driver a Connector declares, and loading it through its manifest.

use std::{
    fmt, fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use adbc_core::{LOAD_FLAG_DEFAULT, options::AdbcVersion};
use adbc_driver_manager::ManagedDriver;
use connector_sdk::{ConnectorError, DataSourceAccess, ErrorKind, LibrarySettings};
use toml::de::{DeTable, DeValue};

/// A driver version reported as `major.minor.patch`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DriverVersion {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl FromStr for DriverVersion {
    type Err = ();

    /// Parses `major.minor.patch`, optionally prefixed with `v` as Go drivers
    /// report their module version, ignoring a pre-release or build suffix
    /// after the patch number.
    fn from_str(text: &str) -> Result<Self, ()> {
        let text = text.strip_prefix('v').unwrap_or(text);
        let mut parts = text.splitn(3, '.');
        let number = |part: Option<&str>| part.and_then(|part| part.parse::<u64>().ok());
        let major = number(parts.next()).ok_or(())?;
        let minor = number(parts.next()).ok_or(())?;
        let patch = parts.next().ok_or(())?;
        let digits = patch
            .find(|character: char| !character.is_ascii_digit())
            .map_or(patch, |end| &patch[..end]);
        let patch = number(Some(digits)).ok_or(())?;
        Ok(Self {
            major,
            minor,
            patch,
        })
    }
}

impl fmt::Display for DriverVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The ADBC driver a Connector uses and the versions it was tested with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DriverRequirement {
    /// The driver's name: its manifest is `<library>.toml` in the library's
    /// ADBC driver directory.
    pub library: &'static str,
    /// The lowest supported version.
    pub minimum: DriverVersion,
    /// The first unsupported version above the supported range.
    pub below: DriverVersion,
}

impl DriverRequirement {
    /// The supported range as published in static information.
    pub fn supported_versions(&self) -> String {
        format!(">={}, <{}", self.minimum, self.below)
    }

    /// The Connector's form of Data Source access, for its descriptor.
    pub fn access(&self) -> DataSourceAccess {
        DataSourceAccess::Adbc {
            driver: self.library.to_owned(),
            supported_versions: self.supported_versions(),
        }
    }

    pub(crate) fn supports(&self, version: DriverVersion) -> bool {
        self.minimum <= version && version < self.below
    }

    /// Loads the driver through its ADBC driver manifest in the library's
    /// driver directory.
    ///
    /// The manifest names the shared library and may declare the driver's
    /// version. A relative library path given as a string `Driver.shared` is
    /// relative to the driver directory, so a directory of drivers can be
    /// moved as a whole; the per-platform table form is left to the driver
    /// manager. A missing directory,
    /// manifest, or library is `invalid settings`, because the directory is a
    /// library setting.
    pub(crate) fn load(&self, settings: &LibrarySettings) -> Result<AdbcDriver, ConnectorError> {
        let Some(directory) = &settings.adbc_driver_directory else {
            return Err(invalid_settings(
                "no ADBC driver directory is configured for the library",
            ));
        };
        let manifest_path = directory.join(format!("{}.toml", self.library));
        let not_loaded = || {
            invalid_settings(format!(
                "the ADBC driver `{}` could not be loaded from its manifest in the driver directory",
                self.library
            ))
        };
        let contents = fs::read_to_string(&manifest_path).map_err(|_| not_loaded())?;
        let manifest = DriverManifest::parse(&contents).ok_or_else(not_loaded)?;
        // The driver manager resolves a relative library path against the
        // working directory, so such a path is resolved here instead.
        let driver = match &manifest.relative_library {
            Some(library) => ManagedDriver::load_dynamic_from_filename(
                directory.join(&library.path),
                library.entrypoint.as_deref().map(str::as_bytes),
                AdbcVersion::V110,
            ),
            None => ManagedDriver::load_from_name(
                &manifest_path,
                None,
                AdbcVersion::V110,
                LOAD_FLAG_DEFAULT,
                None,
            ),
        }
        .map_err(|_| not_loaded())?;
        Ok(AdbcDriver {
            driver,
            requirement: *self,
            manifest_version: manifest.version,
        })
    }
}

/// What this crate reads from an ADBC driver manifest itself; the driver
/// manager reads the rest.
#[derive(Debug, PartialEq, Eq)]
struct DriverManifest {
    /// The top-level `version`, if the manifest declares one.
    version: Option<String>,
    /// `Driver.shared`, when it is a relative path.
    relative_library: Option<RelativeLibrary>,
}

/// A driver's shared library named by a path relative to its manifest.
#[derive(Debug, PartialEq, Eq)]
struct RelativeLibrary {
    path: PathBuf,
    /// `Driver.entrypoint`, which the driver manager otherwise derives from
    /// the library's file name.
    entrypoint: Option<String>,
}

impl DriverManifest {
    /// Parses a manifest; text that is not a manifest, or a `version` or
    /// `entrypoint` that is not a string, is `None`.
    fn parse(contents: &str) -> Option<Self> {
        let manifest = DeTable::parse(contents).ok()?;
        let manifest = manifest.get_ref();
        // The one manifest version the driver manager reads, which it checks
        // itself only when it reads the manifest.
        match manifest
            .get("manifest_version")
            .map(|value| value.get_ref())
        {
            Some(DeValue::Integer(number)) if number.as_str() == "1" => {}
            None => {}
            Some(_) => return None,
        }
        let version = match manifest.get("version").map(|value| value.get_ref()) {
            Some(DeValue::String(version)) => Some(version.to_string()),
            Some(_) => return None,
            None => None,
        };
        let driver = manifest
            .get("Driver")
            .and_then(|driver| driver.get_ref().as_table());
        let relative_library = match driver
            .and_then(|driver| driver.get("shared"))
            .map(|shared| shared.get_ref())
        {
            Some(DeValue::String(path)) if Path::new(path.as_ref()).is_relative() => {
                let entrypoint = match driver
                    .and_then(|driver| driver.get("entrypoint"))
                    .map(|entrypoint| entrypoint.get_ref())
                {
                    Some(DeValue::String(entrypoint)) => Some(entrypoint.to_string()),
                    Some(_) => return None,
                    None => None,
                };
                Some(RelativeLibrary {
                    path: PathBuf::from(path.as_ref()),
                    entrypoint,
                })
            }
            _ => None,
        };
        Some(Self {
            version,
            relative_library,
        })
    }
}

/// A loaded ADBC driver and what its manifest declares.
///
/// Clones share the loaded library, which the driver manager keeps loaded.
#[derive(Clone)]
pub(crate) struct AdbcDriver {
    pub(crate) driver: ManagedDriver,
    pub(crate) requirement: DriverRequirement,
    /// The version the manifest declares, used when the driver itself reports
    /// no version.
    pub(crate) manifest_version: Option<String>,
}

pub(crate) fn invalid_settings(message: impl Into<String>) -> ConnectorError {
    ConnectorError::new(ErrorKind::InvalidSettings, false, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Option<DriverVersion> {
        text.parse().ok()
    }

    fn requirement() -> DriverRequirement {
        DriverRequirement {
            library: "adbc_driver_example",
            minimum: version("1.13.0").unwrap(),
            below: version("2.0.0").unwrap(),
        }
    }

    #[test]
    fn release_versions_parse_and_suffixes_are_ignored() {
        let expected = DriverVersion {
            major: 1,
            minor: 13,
            patch: 0,
        };
        assert_eq!(version("1.13.0"), Some(expected));
        assert_eq!(version("1.13.0-SNAPSHOT"), Some(expected));
        assert_eq!(version("v1.13.0"), Some(expected));
        assert_eq!(version("1.13"), None);
        assert_eq!(version("V1.13.0"), None);
        assert_eq!(version("vv1.13.0"), None);
    }

    #[test]
    fn manifest_versions_are_read_from_the_top_level_key() {
        let version = |contents: &str| DriverManifest::parse(contents).map(|m| m.version);
        let manifest =
            "manifest_version = 1\nversion = \"1.13.0\"\n[Driver]\nshared = \"/lib.so\"\n";
        assert_eq!(version(manifest), Some(Some("1.13.0".to_owned())));
        assert_eq!(version("manifest_version = 1\n"), Some(None));
        assert_eq!(version("version = 1"), None);
        assert_eq!(version("not toml ="), None);
    }

    #[test]
    fn only_a_relative_shared_library_is_resolved_by_the_manifest() {
        let library = |contents: &str| DriverManifest::parse(contents).map(|m| m.relative_library);
        assert_eq!(library("[Driver]\nshared = \"/lib.so\"\n"), Some(None));
        assert_eq!(
            library("[Driver]\nshared = \"lib/driver.so\"\nentrypoint = \"Init\"\n"),
            Some(Some(RelativeLibrary {
                path: PathBuf::from("lib/driver.so"),
                entrypoint: Some("Init".to_owned()),
            }))
        );
        assert_eq!(
            library("[Driver]\nshared = \"lib/driver.so\"\nentrypoint = 1\n"),
            None
        );
        // An unsupported manifest version is rejected whichever path loads it.
        assert_eq!(
            library("manifest_version = 2\n[Driver]\nshared = \"lib/driver.so\"\n"),
            None
        );
    }

    #[test]
    fn a_driver_missing_from_the_library_settings_is_invalid_settings() {
        let empty_directory = std::env::temp_dir().join("connector-adbc-no-drivers");
        std::fs::create_dir_all(&empty_directory).unwrap();
        for settings in [
            LibrarySettings::default(),
            LibrarySettings {
                adbc_driver_directory: Some(empty_directory),
            },
        ] {
            let Err(error) = requirement().load(&settings) else {
                panic!("no driver can be loaded");
            };
            assert_eq!(error.kind(), ErrorKind::InvalidSettings);
        }
    }

    #[test]
    fn the_supported_range_includes_its_minimum_and_excludes_its_bound() {
        let requirement = requirement();
        assert_eq!(requirement.supported_versions(), ">=1.13.0, <2.0.0");
        assert!(requirement.supports(version("1.13.0").unwrap()));
        assert!(requirement.supports(version("1.99.9").unwrap()));
        assert!(!requirement.supports(version("1.12.9").unwrap()));
        assert!(!requirement.supports(version("2.0.0").unwrap()));
    }
}
