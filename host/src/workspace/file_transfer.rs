use std::sync::Arc;
use std::path::{Path, PathBuf};

use wire::file_transfer::Error;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Conflict {
    pub modified: i64,
    pub size: u64,
}

pub trait Upload: Send {
    fn conflict(&mut self, seated: &dyn Fn() -> bool) -> Result<Option<Conflict>, Error>;
    fn accept_conflict(&mut self, seated: &dyn Fn() -> bool) -> Result<(), Error>;
    fn prepare(&mut self, seated: &dyn Fn() -> bool) -> Result<u64, Error>;
    fn write(&mut self, bytes: &[u8], seated: &dyn Fn() -> bool) -> Result<(), Error>;
    fn verify(&mut self, sha256: &str, seated: &dyn Fn() -> bool) -> Result<(), Error>;
    fn publish(&mut self) -> Result<bool, Error>;
}

pub trait Download: Send {
    fn size(&self) -> u64;
    fn rewind(&mut self, seated: &dyn Fn() -> bool) -> Result<(), Error>;
    fn read(&mut self, bytes: &mut [u8], seated: &dyn Fn() -> bool) -> Result<(), Error>;
    fn finish(&mut self, seated: &dyn Fn() -> bool) -> Result<(), Error>;
}

type PathOp = dyn Fn(&[String], &str, bool, &dyn Fn() -> bool) -> Result<PathBuf, Error> + Send + Sync;
type UploadOp = dyn Fn(&Path, u64, &dyn Fn() -> bool) -> Result<Box<dyn Upload>, Error> + Send + Sync;
type DownloadOp = dyn Fn(&Path, &dyn Fn() -> bool) -> Result<Box<dyn Download>, Error> + Send + Sync;

#[derive(Clone)]
pub struct FileTransfers {
    pub path: Arc<PathOp>,
    pub upload: Arc<UploadOp>,
    pub download: Arc<DownloadOp>,
}
