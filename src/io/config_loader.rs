//! This module contains code that handles configuration loading.

use std::{
    ffi::OsString,
    fmt::Display,
    fs::read_to_string,
    io,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

use log::warn;
use serde::{Deserialize, Serialize};

/// The default path of `sslocal` binary, if not defined by profile
const SSLOCAL_DEFAULT_PATH: &str = "sslocal";
/// The existence of this file in a directory indicates that
/// this directory is a connection profile.
const PROFILE_DEF_FILE_NAME: &str = "profile.yaml";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigProfileSerde {
    pub display_name: Option<String>,
    pub pwd: Option<PathBuf>,
    pub ss_bin_path: Option<PathBuf>,
    pub ss_config_path: Option<PathBuf>,
    pub extra_args: Option<Vec<String>>,
}

impl TryInto<ConfigProfile> for ConfigProfileSerde {
    type Error = String;

    fn try_into(self) -> Result<ConfigProfile, Self::Error> {
        let Self {
            display_name,
            pwd,
            ss_bin_path,
            ss_config_path,
            extra_args,
        } = self;

        let display_name = display_name.ok_or("display_name not set".to_string())?;
        let pwd = pwd.ok_or("pwd not set".to_string())?;
        let ss_bin_path = ss_bin_path.ok_or("ss_bin_path not set".to_string())?;

        Ok(ConfigProfile {
            display_name,
            pwd,
            ss_bin_path,
            ss_config_path,
            extra_args,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ConfigProfile {
    pub display_name: String,
    pub pwd: PathBuf,
    pub ss_bin_path: PathBuf,
    pub ss_config_path: Option<PathBuf>,
    pub extra_args: Option<Vec<String>>,
}

impl ConfigProfile {
    /// Run `sslocal` using the settings specified by this profile.
    ///
    /// If `stdout` or `stderr` is `None`, the corresponding output
    /// is redirected to`Stdio::null()` (discarded) by default.
    pub fn run_sslocal<O, E>(&self, stdout: Option<O>, stderr: Option<E>) -> io::Result<Child>
    where
        O: Into<Stdio>,
        E: Into<Stdio>,
    {
        let config_args: Vec<OsString> = self
            .ss_config_path
            .as_ref()
            .map_or(vec![], |p| vec!["--config".into(), p.into()]);
        let extra_args = self.extra_args.clone().unwrap_or(Vec::new()); // better would be to return a slice but I can't be arsed
        let stdout = stdout.map_or(Stdio::null(), |o| o.into());
        let stderr = stderr.map_or(Stdio::null(), |e| e.into());

        Command::new(self.ss_bin_path.clone())
            .current_dir(self.pwd.clone())
            .args(config_args)
            .args(extra_args)
            .stdin(Stdio::null()) // sslocal does not read from stdin
            .stdout(stdout)
            .stderr(stderr)
            .spawn()
    }
}

#[derive(Debug, Clone)]
pub struct ConfigGroup {
    pub display_name: String,
    pub content: Vec<ConfigFolder>,
}

#[derive(Debug)]
pub enum ConfigLoadError {
    /// Each profile should be its own directory, which can be placed under other directories to form groups.
    NotDirectory(String),
    /// The profile definition file cannot be parsed.
    ProfileParseError(serde_yaml::Error),
    /// The directory contains files (which means it's considered a profile folder),
    /// but there's no profile definition file.
    NoProfileDef(String),
    /// The directory contains neither files nor other valid profiles.
    EmptyGroup(String),
    /// The filesystem encountered an IOError.
    IOError(io::Error),
}

impl Display for ConfigLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use ConfigLoadError::*;

        match self {
            NotDirectory(s) => write!(f, "ConfigLoadError-NotDirectory: {}", s),
            ProfileParseError(e) => write!(f, "ConfigLoadError-ProfileParseError: {}", e),
            NoProfileDef(s) => write!(f, "ConfigLoadError-NoProfileDef: {}", s),
            EmptyGroup(s) => write!(f, "ConfigLoadError-EmptyGroup: {}", s),
            IOError(e) => write!(f, "ConfigLoadError-IOError: {}", e),
        }
    }
}

impl From<serde_yaml::Error> for ConfigLoadError {
    fn from(err: serde_yaml::Error) -> Self {
        Self::ProfileParseError(err)
    }
}
impl From<io::Error> for ConfigLoadError {
    fn from(err: io::Error) -> Self {
        Self::IOError(err)
    }
}

#[derive(Debug, Clone)]
pub enum ConfigFolder {
    /// A single `sslocal` connection profile.
    Profile(ConfigProfile),
    /// A group containing multiple profiles and/or subgroups.
    Group(ConfigGroup),
}

impl ConfigFolder {
    /// Recursively loads all nested profiles within the specified directory.
    ///
    /// **Symlinking is not currently supported.**
    ///
    /// If a call to this function with the user-specified base path fails,
    /// then run the program as if there are no existing configs.
    pub fn from_path_recurse<P>(path: P) -> Result<Self, ConfigLoadError>
    where
        P: AsRef<Path>,
    {
        let path = path.as_ref().canonicalize()?;
        let full_path_str = path.to_str().ok_or(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Path is not valid UTF-8: {:?}", path),
        ))?;

        // make sure path is a directory
        if !path.is_dir() {
            return Err(ConfigLoadError::NotDirectory(full_path_str.into()));
        }
        // use directory name as folder's display name
        let display_name = path
            .file_name()
            .unwrap() // path has already been canonicalised
            .to_str()
            .unwrap() // UTF-8 has already been verified
            .to_string();

        // if directory contains the profile definition file, then consider it a profile
        let mut profile_yaml_path = path.clone();
        profile_yaml_path.push(PROFILE_DEF_FILE_NAME);
        if profile_yaml_path.is_file() {
            let content = read_to_string(profile_yaml_path)?;
            let mut profile: ConfigProfileSerde = serde_yaml::from_str(&content)?;
            // use directory name as default display name
            profile.display_name.get_or_insert(display_name);
            // set pwd correctly
            profile.pwd = Some(profile.pwd.map_or(
                path.clone(), // use current profile path as default pwd
                |p| {
                    let mut pwd = path.clone(); // use current profile path as base
                    pwd.push(p); // this handles both relative and absolute path
                    pwd
                },
            ));
            // set default binary path
            profile.ss_bin_path.get_or_insert(SSLOCAL_DEFAULT_PATH.into());
            return Ok(Self::Profile(
                profile.try_into().unwrap(), // required fields are set
            ));
        }

        // otherwise, check if it contains files at all
        // if so consider it a profile that's missing a definition file
        let has_files = path.read_dir()?.any(|ent_res| match ent_res {
            Ok(ent) => ent.path().is_file(),
            Err(err) => {
                warn!("Cannot open a file or directory: {}", err);
                false
            }
        });
        if has_files {
            return Err(ConfigLoadError::NoProfileDef(full_path_str.into()));
        }

        // otherwise, consider it a group
        let mut subdirs = vec![];
        for ent_res in path.read_dir()? {
            // recursively load all subdirectories
            match ent_res {
                Ok(ent) => match Self::from_path_recurse(ent.path()) {
                    Ok(cf) => subdirs.push(cf),
                    Err(err) => warn!("Cannot load a subdirectory: {}", err),
                },
                Err(err) => warn!("Cannot open a file or directory: {}", err),
            }
        }
        if subdirs.is_empty() {
            Err(ConfigLoadError::EmptyGroup(full_path_str.into()))
        } else {
            Ok(ConfigFolder::Group(ConfigGroup {
                display_name,
                content: subdirs,
            }))
        }
    }

    /// Recursively count the number of nested profiles within this `ConfigFolder`.
    pub fn profile_count(&self) -> usize {
        use ConfigFolder::*;
        match self {
            Profile(_) => 1,
            Group(g) => g.content.iter().map(|cf| cf.profile_count()).sum(),
        }
    }

    /// Recursively get all the nested profiles within this `ConfigFolder`,
    /// flattened and returned by reference.
    pub fn get_profiles(&self) -> Vec<&ConfigProfile> {
        use ConfigFolder::*;
        match self {
            Profile(p) => vec![p],
            Group(g) => g.content.iter().flat_map(|cf| cf.get_profiles()).collect(),
        }
    }
}
