//! Byte credit shared by the terminal and file channels.

#[derive(Default)]
pub(super) struct Credit {
    outstanding: usize,
}

impl Credit {
    pub fn outstanding(&self) -> usize {
        self.outstanding
    }

    pub fn available(&self, window: usize) -> usize {
        window - self.outstanding
    }

    pub fn take(&mut self, bytes: usize, window: usize) -> bool {
        if bytes == 0 || bytes > self.available(window) {
            return false;
        }
        self.outstanding += bytes;
        true
    }

    pub fn acknowledge(&mut self, bytes: usize) -> bool {
        if bytes == 0 || bytes > self.outstanding {
            return false;
        }
        self.outstanding -= bytes;
        true
    }
}
