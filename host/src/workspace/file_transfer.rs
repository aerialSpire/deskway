use std::fs::Metadata;
use std::path::{Path, PathBuf};

use wire::file_transfer::Error;

pub struct Conflict {
    pub modified: i64,
    pub metadata: Metadata,
}

pub trait Upload: Send {
    fn conflict(&self, seated: &dyn Fn() -> bool) -> Result<Option<Conflict>, Error>;
    fn prepare(&mut self, seated: &dyn Fn() -> bool) -> Result<u64, Error>;
    fn write(&mut self, bytes: &[u8], seated: &dyn Fn() -> bool) -> Result<(), Error>;
    fn verify(&self, sha256: &str, seated: &dyn Fn() -> bool) -> Result<(), Error>;
    fn publish(&self, approval: Option<&Conflict>) -> Result<bool, Error>;
}

pub trait Download: Send {
    fn size(&self) -> u64;
    fn rewind(&mut self, seated: &dyn Fn() -> bool) -> Result<(), Error>;
    fn read(&mut self, bytes: &mut [u8], seated: &dyn Fn() -> bool) -> Result<(), Error>;
    fn finish(&mut self, seated: &dyn Fn() -> bool) -> Result<(), Error>;
}

#[derive(Clone, Copy)]
pub struct FileTransfers {
    pub path: fn(&[String], &str, bool, &dyn Fn() -> bool) -> Result<PathBuf, Error>,
    pub upload: fn(&Path, u64, &dyn Fn() -> bool) -> Result<Box<dyn Upload>, Error>,
    pub download: fn(&Path, &dyn Fn() -> bool) -> Result<Box<dyn Download>, Error>,
}
