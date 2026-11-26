//! The running host records its process id in the lock file it holds, once it receives signals.
use std::fs::File;
use std::io::{self, Read, Seek, Write};

pub(super) fn record_pid(file: &mut File) -> io::Result<()> {
    file.rewind()?;
    writeln!(file, "{}", std::process::id())
}

fn read_pid(file: impl Read) -> io::Result<libc::pid_t> {
    let mut pid = String::new();
    file.take(16).read_to_string(&mut pid)?;
    pid.trim_end().parse::<libc::pid_t>().ok().filter(|pid| *pid > 0)
        .ok_or_else(|| io::ErrorKind::InvalidData.into())
}

/// Ask the host that holds the lock on `file` to stop AI control.
pub(super) fn signal_stop(file: &File) -> io::Result<()> {
    if unsafe { libc::kill(read_pid(file)?, libc::SIGUSR1) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
#[test]
fn a_lock_pid_is_a_positive_process_id() {
    for (text, want) in [
        ("123\n", Some(123)),
        ("", None),
        ("0\n", None),
        ("-1\n", None),
        ("123\n456\n", None),
    ] {
        assert_eq!(read_pid(text.as_bytes()).ok(), want, "{text:?}");
    }
}
