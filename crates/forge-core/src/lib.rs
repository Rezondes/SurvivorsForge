//! Logic shared by the SurvivorsForge launcher, API, website and scanner. No GUI dependencies.

pub mod archive;
pub mod compat;
pub mod deps;
pub mod github_web;
pub mod markdown;
pub mod pack;
pub mod rate_limit;
pub mod registry;
pub mod roles;
pub mod toc;
pub mod trust;

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    Io(String),
    Zip(String),
    UnsafePath(String),
    NoAddonFound,
    Parse(String),
    /// Two addons of a pack use the same folder.
    FolderTwice(String),
    /// A pack would pass this many bytes.
    TooLarge(u64),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "File error: {e}"),
            Error::Zip(e) => write!(f, "The zip file is damaged or invalid: {e}"),
            Error::UnsafePath(p) => write!(f, "The zip contains an unsafe path ({p}), installation cancelled"),
            Error::NoAddonFound => write!(f, "No addon found"),
            Error::Parse(e) => write!(f, "Unexpected response: {e}"),
            Error::FolderTwice(folder) => write!(f, "Two addons use the folder {folder}"),
            Error::TooLarge(max) => write!(f, "The pack would be larger than {} MB", max / 1_000_000),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
