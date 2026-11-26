use std::path::{Path, PathBuf};

use wire::file_transfer::Error;

pub trait Upload {
    fn write(&mut self, bytes: &[u8], seated: &dyn Fn() -> bool) -> Result<(), Error>;
    fn commit(self: Box<Self>, seated: &dyn Fn() -> bool) -> Result<(), Error>;
}

pub trait Download {
    fn size(&self) -> u64;
    fn read(&mut self, bytes: &mut [u8], seated: &dyn Fn() -> bool) -> Result<(), Error>;
    fn verify(&self, seated: &dyn Fn() -> bool) -> Result<(), Error>;
}

#[derive(Clone, Copy)]
pub struct FileTransfers {
    pub path: fn(&[String], &str, bool) -> Result<PathBuf, Error>,
    pub upload: fn(&Path, u64, &dyn Fn() -> bool) -> Result<Box<dyn Upload>, Error>,
    pub download: fn(&Path, &dyn Fn() -> bool) -> Result<Box<dyn Download>, Error>,
}
