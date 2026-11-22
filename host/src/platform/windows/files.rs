//! What file transfer needs from Windows. The agent runs as SYSTEM; files are written as the
//! person signed in at the console, never as SYSTEM.

use std::path::{Path, PathBuf};

use super::user;
use crate::platform::Desk;

pub struct Files;

impl Files {
    pub fn new() -> Self {
        Files
    }
}

impl crate::transfer::Host for Files {
    fn locked(&mut self) -> bool {
        super::desktop::input() != Desk::Default
    }
    fn downloads(&mut self) -> Option<PathBuf> {
        user::downloads()
    }
    fn create(&mut self, path: &Path) -> std::io::Result<std::fs::File> {
        user::as_user(|| {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::OpenOptions::new().write(true).create_new(true).open(path)
        })
    }
    fn remove(&mut self, path: &Path) {
        let _ = user::as_user(|| std::fs::remove_file(path));
    }
}
