use std::path::{Path, PathBuf};

use wire::file_transfer::Error;

pub trait Upload: Send {
    fn conflict(&self, seated: &dyn Fn() -> bool) -> Result<Option<(u64, i64)>, Error>;
    fn prepare(&mut self, seated: &dyn Fn() -> bool) -> Result<u64, Error>;
    fn write(&mut self, bytes: &[u8], seated: &dyn Fn() -> bool) -> Result<(), Error>;
    fn commit(&mut self, sha256: &str, replace: bool, seated: &dyn Fn() -> bool) -> Result<bool, Error>;
    fn abandon(&mut self);
}

pub trait Download: Send {
    fn size(&self) -> u64;
    fn rewind(&mut self, seated: &dyn Fn() -> bool) -> Result<(), Error>;
    fn read(&mut self, bytes: &mut [u8], seated: &dyn Fn() -> bool) -> Result<(), Error>;
    fn verify(&self, seated: &dyn Fn() -> bool) -> Result<(), Error>;
}

#[derive(Clone, Copy)]
pub struct FileTransfers {
    pub path: fn(&[String], &str, bool, &dyn Fn() -> bool) -> Result<PathBuf, Error>,
    pub upload: fn(&Path, u64, &dyn Fn() -> bool) -> Result<Box<dyn Upload>, Error>,
    pub download: fn(&Path, &dyn Fn() -> bool) -> Result<Box<dyn Download>, Error>,
}
