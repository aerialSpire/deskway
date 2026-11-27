//! Local pipes between Deskway's own processes.
//!
//! Pipe names and process ids can be faked by any local program, so neither end trusts them.
//! Each pipe has a DACL that admits only the expected accounts, is created with
//! `FILE_FLAG_FIRST_PIPE_INSTANCE` so a squatter makes creation fail instead of being used,
//! rejects remote clients, and (for the agent pipe) both ends prove knowledge of a shared key
//! before anything else is sent.
//!
//! Every message is one length-prefixed frame. On the agent pipe it holds a value in bincode 1
//! (variable-length integers, byte strings as length plus raw bytes): video frames and file
//! chunks are most of that traffic, so they cross without the size and parsing cost of a text format. Measured for
//! a 500 KB frame (release build, one test at a time): JSON with base64 was 666 680 bytes,
//! 777 µs to encode and 383 µs to decode; bincode is 500 006 bytes, about 7 µs each way (see
//! `a_video_frame_costs_little_to_encode`). bincode 1 needs only serde and is stable; postcard
//! would serve as well but brings several no_std crates for nothing here. The panel pipe carries
//! small status messages as JSON, which tools and the e2e tests read as they are.

use anyhow::{bail, ensure, Context, Result};
use bincode::Options;
use hmac::{Hmac, Mac};
use serde::de::DeserializeOwned;
use serde::Serialize;
use sha2::Sha256;
use windows::core::{HSTRING, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Security::Authorization::*;
use windows::Win32::Security::*;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::Pipes::*;
use windows::Win32::System::Threading::*;
use windows::Win32::System::IO::*;

pub const AGENT_PIPE: &str = r"\\.\pipe\deskway-agent";
pub const UI_PIPE: &str = r"\\.\pipe\deskway-ui";
pub const WORKER_PIPE: &str = r"\\.\pipe\deskway-worker";
pub const WORKER_ADMIN_PIPE: &str = r"\\.\pipe\deskway-worker-admin";

/// Dev mode (tests, running as the signed-in user) uses names of its own, so it never meets
/// an installed service's pipes. Derived from the config folder, which both dev processes share.
pub fn pipe_name(base: &str, dev: bool) -> String {
    if !dev {
        return base.to_string();
    }
    let dir = crate::config::dir().map(|d| d.display().to_string()).unwrap_or_default();
    let h = <sha2::Sha256 as sha2::Digest>::digest(dir.as_bytes());
    format!("{base}-dev-{:02x}{:02x}{:02x}{:02x}", h[0], h[1], h[2], h[3])
}
/// Messages hold one encoded video frame at most; anything larger is a broken peer.
const MAX_FRAME: usize = 16 << 20;
const BUFFER: u32 = 1 << 20;
const SHARED_INSTANCES: u32 = 8;

const TAG_JSON: u8 = 0;
const TAG_BINARY: u8 = 1;
const TAG_HANDSHAKE: u8 = 2;

/// The SID of the account this process runs as, e.g. `S-1-5-18` for SYSTEM.
pub fn own_sid() -> Result<String> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
        let token = Event(token);
        token_sid(token.0)
    }
}

fn token_sid(token: HANDLE) -> Result<String> {
    unsafe {
        let mut buf = [0usize; 32];
        let mut len = 0u32;
        GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr() as _), size_of_val(&buf) as u32, &mut len)?;
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut s = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut s)?;
        let out = s.to_string();
        let _ = LocalFree(Some(HLOCAL(s.0 as _)));
        Ok(out?)
    }
}

fn token_is_admin(token: HANDLE) -> Result<bool> {
    unsafe {
        let mut len = 0;
        let _ = GetTokenInformation(token, TokenGroups, None, 0, &mut len);
        let mut buf = vec![0usize; (len as usize).div_ceil(size_of::<usize>())];
        GetTokenInformation(token, TokenGroups, Some(buf.as_mut_ptr() as _), len, &mut len)?;
        let groups = &*(buf.as_ptr() as *const TOKEN_GROUPS);
        // A UAC-filtered administrator still belongs to the group (marked deny-only).
        let groups = std::slice::from_raw_parts(groups.Groups.as_ptr(), groups.GroupCount as usize);
        Ok(groups.iter().any(|group| IsWellKnownSid(group.Sid, WinBuiltinAdministratorsSid).as_bool()))
    }
}

fn permits_admin_link(owner: Option<&str>, user: &str, admin: bool) -> bool {
    admin || owner == Some(user)
}

#[cfg(test)]
#[test]
fn admin_link_is_limited_to_the_recorded_user_and_administrators() {
    assert!(permits_admin_link(Some("S-1-5-21-1001"), "S-1-5-21-1001", false));
    assert!(permits_admin_link(Some("S-1-5-21-1001"), "S-1-5-21-1002", true));
    assert!(!permits_admin_link(Some("S-1-5-21-1001"), "S-1-5-21-1002", false));
    assert!(!permits_admin_link(None, "S-1-5-21-1002", false));
    assert!(permits_admin_link(None, "S-1-5-21-1002", true));
}

#[cfg(test)]
#[test]
fn admin_link_uses_the_pipe_clients_windows_token() {
    let sid = own_sid().unwrap();
    let name = format!(r"\\.\pipe\deskway-ui-auth-test-{}", std::process::id());
    let server = Pipe::create(&name, &format!("D:P(A;;GA;;;{sid})")).unwrap();
    let client = std::thread::spawn(move || {
        let pipe = Pipe::connect(&name).unwrap();
        pipe.send_json(&"S-1-5-21-1234").unwrap();
        pipe.recv_json::<bool>().unwrap()
    });
    server.accept().unwrap();
    let forged = server.recv_json::<String>().unwrap();
    assert!(server.allows_admin_link(Some(&sid)).unwrap());
    let mut token = Event(HANDLE::default());
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token.0).unwrap(); }
    let admin = token_is_admin(token.0).unwrap();
    assert_eq!(server.allows_admin_link(Some(&forged)).unwrap(), admin);
    server.send_json(&true).unwrap();
    assert!(client.join().unwrap());
}

pub fn running_as_system() -> bool {
    own_sid().is_ok_and(|s| s == "S-1-5-18")
}

/// A pipe handle opened for overlapped I/O, so one thread can read while another writes.
pub struct Pipe {
    h: HANDLE,
    /// One frame at a time, from whichever thread sends it.
    writing: std::sync::Mutex<()>,
    /// A write did not finish in time: the stream may hold half a frame, so this connection
    /// is over. The other end sees it end and connects again.
    broken: std::sync::atomic::AtomicBool,
}

unsafe impl Send for Pipe {}
unsafe impl Sync for Pipe {}

impl Drop for Pipe {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.h);
        }
    }
}

struct Event(HANDLE);
impl Drop for Event {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// How long a write may wait for the other end to read. Past this the other end is stuck,
/// and waiting longer would stall everything behind this write.
static WRITE_TIMEOUT_MS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(10_000);

impl Pipe {
    fn from(h: HANDLE) -> Pipe {
        Pipe { h, writing: std::sync::Mutex::new(()), broken: std::sync::atomic::AtomicBool::new(false) }
    }

    /// Creates the only instance of a pipe. Fails if any other process already holds the name.
    pub fn create(name: &str, sddl: &str) -> Result<Pipe> {
        Self::create_inner(name, sddl, true, 1)
    }

    /// Creates the first instance of a pipe that serves several clients.
    pub fn create_shared(name: &str, sddl: &str) -> Result<Pipe> {
        Self::create_inner(name, sddl, true, SHARED_INSTANCES)
    }

    /// Another instance of a pipe this process already holds open.
    pub fn create_more(name: &str, sddl: &str) -> Result<Pipe> {
        Self::create_inner(name, sddl, false, SHARED_INSTANCES)
    }

    /// The process that created the pipe this client is connected to.
    pub fn server_pid(&self) -> Result<u32> {
        let mut pid = 0u32;
        unsafe { GetNamedPipeServerProcessId(self.h, &mut pid)? };
        Ok(pid)
    }

    fn create_inner(name: &str, sddl: &str, first: bool, instances: u32) -> Result<Pipe> {
        unsafe {
            let mut sd = PSECURITY_DESCRIPTOR::default();
            ConvertStringSecurityDescriptorToSecurityDescriptorW(&HSTRING::from(sddl), SDDL_REVISION_1, &mut sd, None).context("pipe security descriptor")?;
            let sa = SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd.0, bInheritHandle: false.into() };
            let h = CreateNamedPipeW(
                &HSTRING::from(name),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { FILE_FLAGS_AND_ATTRIBUTES(0) },
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                instances,
                BUFFER,
                BUFFER,
                0,
                Some(&sa),
            );
            let _ = LocalFree(Some(HLOCAL(sd.0)));
            if h.is_invalid() {
                bail!("cannot create {name} (another process may hold it): {}", windows::core::Error::from_thread());
            }
            Ok(Pipe::from(h))
        }
    }

    /// Drops the current client but keeps the pipe, and so the name, for the next one.
    pub fn disconnect(&self) {
        unsafe {
            // A stuck client would never read what is left: no waiting for it then.
            if !self.broken.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = FlushFileBuffers(self.h);
            }
            let _ = DisconnectNamedPipe(self.h);
        }
    }

    /// Ends this connection after a stuck write: every read or write waiting on it fails, so
    /// the loops on both ends finish and connect again.
    fn cut(&self) {
        self.broken.store(true, std::sync::atomic::Ordering::Relaxed);
        unsafe {
            let _ = CancelIoEx(self.h, None);
            let _ = DisconnectNamedPipe(self.h);
        }
    }

    /// Waits for a client.
    pub fn accept(&self) -> Result<()> {
        self.broken.store(false, std::sync::atomic::Ordering::Relaxed);
        unsafe {
            let ev = Event(CreateEventW(None, true, false, None)?);
            let mut ov = OVERLAPPED { hEvent: ev.0, ..Default::default() };
            match ConnectNamedPipe(self.h, Some(&mut ov)) {
                Ok(()) => Ok(()),
                Err(e) if e.code() == ERROR_PIPE_CONNECTED.to_hresult() => Ok(()),
                Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => {
                    let mut n = 0;
                    GetOverlappedResult(self.h, &ov, &mut n, true).context("accept")?;
                    Ok(())
                }
                Err(e) => Err(e.into()),
            }
        }
    }

    /// Opens an existing pipe. Identification level only: a fake server cannot act as us.
    pub fn connect(name: &str) -> Result<Pipe> {
        unsafe {
            let h = CreateFileW(
                &HSTRING::from(name),
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                None,
            )?;
            Ok(Pipe::from(h))
        }
    }

    fn io(&self, buf: &mut [u8], write: bool) -> Result<usize> {
        unsafe {
            let ev = Event(CreateEventW(None, true, false, None)?);
            let mut ov = OVERLAPPED { hEvent: ev.0, ..Default::default() };
            let r = if write { WriteFile(self.h, Some(buf), None, Some(&mut ov)) } else { ReadFile(self.h, Some(buf), None, Some(&mut ov)) };
            match r {
                Ok(()) => {}
                Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => {}
                Err(e) => return Err(e.into()),
            }
            let mut n = 0u32;
            if write && WaitForSingleObject(ev.0, WRITE_TIMEOUT_MS.load(std::sync::atomic::Ordering::Relaxed)) == WAIT_TIMEOUT {
                // Cancelled, and waited for: the system is done with `ov` and `buf` after this.
                let _ = CancelIoEx(self.h, Some(&ov));
                let _ = GetOverlappedResult(self.h, &ov, &mut n, true);
                self.cut();
                bail!("pipe write timed out: the other end is not reading");
            }
            GetOverlappedResult(self.h, &ov, &mut n, true)?;
            Ok(n as usize)
        }
    }

    fn read_exact(&self, mut buf: &mut [u8]) -> Result<()> {
        while !buf.is_empty() {
            let n = self.io(buf, false)?;
            ensure!(n > 0, "pipe closed");
            buf = &mut buf[n..];
        }
        Ok(())
    }

    fn write_all(&self, mut data: Vec<u8>) -> Result<()> {
        ensure!(!self.broken.load(std::sync::atomic::Ordering::Relaxed), "pipe closed after a stuck write");
        let _one = self.writing.lock().unwrap_or_else(|e| e.into_inner());
        let mut off = 0;
        while off < data.len() {
            let n = self.io(&mut data[off..], true)?;
            ensure!(n > 0, "pipe closed");
            off += n;
        }
        Ok(())
    }

    /// Writes a frame built by `frame`, whose first five bytes are left for the header.
    fn write_frame(&self, tag: u8, mut b: Vec<u8>) -> Result<()> {
        let len = b.len() - 5;
        ensure!(len <= MAX_FRAME, "frame too large");
        b[..4].copy_from_slice(&(len as u32).to_le_bytes());
        b[4] = tag;
        self.write_all(b)
    }

    fn read_frame(&self) -> Result<(u8, Vec<u8>)> {
        let mut head = [0u8; 5];
        self.read_exact(&mut head)?;
        let len = u32::from_le_bytes(head[..4].try_into().unwrap()) as usize;
        ensure!(len <= MAX_FRAME, "frame too large");
        let mut payload = vec![0u8; len];
        self.read_exact(&mut payload)?;
        Ok((head[4], payload))
    }

    /// One binary message (the agent pipe).
    pub fn send<T: Serialize>(&self, m: &T) -> Result<()> {
        self.write_frame(TAG_BINARY, encode(m)?)
    }

    pub fn recv<T: DeserializeOwned>(&self) -> Result<T> {
        let (tag, payload) = self.read_frame()?;
        ensure!(tag == TAG_BINARY, "unexpected frame type {tag}");
        decode(&payload)
    }

    /// One JSON message (the panel pipe).
    pub fn send_json<T: Serialize>(&self, m: &T) -> Result<()> {
        let mut b = vec![0u8; 5];
        serde_json::to_writer(&mut b, m)?;
        self.write_frame(TAG_JSON, b)
    }

    pub fn recv_json<T: DeserializeOwned>(&self) -> Result<T> {
        let (tag, payload) = self.read_frame()?;
        ensure!(tag == TAG_JSON, "unexpected frame type {tag}");
        serde_json::from_slice(&payload).context("bad message")
    }

    /// The authenticated identity of the last panel message, never a SID supplied in JSON.
    pub fn allows_admin_link(&self, owner: Option<&str>) -> Result<bool> {
        unsafe {
            ImpersonateNamedPipeClient(self.h)?;
            let mut token = HANDLE::default();
            let opened = OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, true, &mut token);
            // Never continue the service loop under a client's token.
            if RevertToSelf().is_err() {
                std::process::abort();
            }
            opened?;
            let token = Event(token);
            Ok(permits_admin_link(owner, &token_sid(token.0)?, token_is_admin(token.0)?))
        }
    }

    fn send_raw(&self, b: &[u8]) -> Result<()> {
        let mut frame = vec![0u8; 5];
        frame.extend_from_slice(b);
        self.write_frame(TAG_HANDSHAKE, frame)
    }

    fn recv_raw(&self, len: usize) -> Result<Vec<u8>> {
        let (tag, payload) = self.read_frame()?;
        ensure!(tag == TAG_HANDSHAKE && payload.len() == len, "bad handshake");
        Ok(payload)
    }
}

/// The one encoding of every message. Lengths inside a message are bounded too, so a broken
/// peer cannot make the reader allocate more than a frame.
fn encoding() -> impl Options {
    bincode::DefaultOptions::new().with_limit(MAX_FRAME as u64)
}

/// A message as a frame body, after five bytes left for the frame header.
fn encode<T: Serialize>(m: &T) -> Result<Vec<u8>> {
    let mut b = Vec::with_capacity(5 + encoding().serialized_size(m)? as usize);
    b.extend_from_slice(&[0; 5]);
    encoding().serialize_into(&mut b, m)?;
    Ok(b)
}

fn decode<T: DeserializeOwned>(payload: &[u8]) -> Result<T> {
    encoding().deserialize(payload).context("bad message")
}

type HmacSha256 = Hmac<Sha256>;

fn proof(key: &[u8], label: &str, a: &[u8], b: &[u8]) -> HmacSha256 {
    let mut m = HmacSha256::new_from_slice(key).expect("any key length");
    m.update(label.as_bytes());
    m.update(a);
    m.update(b);
    m
}

/// Server side: proves itself first, then checks the client.
pub fn handshake_server(p: &Pipe, key: &[u8]) -> Result<()> {
    let client_nonce = p.recv_raw(32)?;
    let server_nonce = wire::crypto::random::<32>();
    let mut reply = server_nonce.to_vec();
    reply.extend_from_slice(&proof(key, "deskway-ipc-server", &client_nonce, &server_nonce).finalize().into_bytes());
    p.send_raw(&reply)?;
    let tag = p.recv_raw(32)?;
    proof(key, "deskway-ipc-client", &client_nonce, &server_nonce).verify_slice(&tag).map_err(|_| anyhow::anyhow!("client does not know the pipe key"))
}

/// Client side: sends nothing of value until the server has proved it knows the key.
pub fn handshake_client(p: &Pipe, key: &[u8]) -> Result<()> {
    let client_nonce = wire::crypto::random::<32>();
    p.send_raw(&client_nonce)?;
    let reply = p.recv_raw(64)?;
    let (server_nonce, tag) = reply.split_at(32);
    proof(key, "deskway-ipc-server", &client_nonce, server_nonce).verify_slice(tag).map_err(|_| anyhow::anyhow!("pipe server does not know the pipe key"))?;
    p.send_raw(&proof(key, "deskway-ipc-client", &client_nonce, server_nonce).finalize().into_bytes())
}

/// Reads the pipe key, creating it if it does not exist yet.
pub fn load_key(path: &std::path::Path, create: bool) -> Result<Vec<u8>> {
    match std::fs::read(path) {
        Ok(k) if k.len() == 32 => Ok(k),
        Ok(_) if !create => bail!("pipe key is damaged"),
        Err(e) if !create => Err(e).context("read pipe key"),
        _ => {
            let k = wire::crypto::random::<32>().to_vec();
            crate::config::write_private(path, &k)?;
            Ok(k)
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::DisplayInfo;
    use crate::platform::{Bytes, Call, Desk, PlatformEvent};
    use wire::types::{ClipboardError, FormatKind};
    use crate::transfer::Note;
    use crate::video::CursorShape;
    use std::sync::Arc;

    fn pair(name: &str) -> (Pipe, Pipe) {
        let sddl = format!("D:P(A;;GA;;;{})", own_sid().unwrap());
        let server = Pipe::create(name, &sddl).unwrap();
        let name2 = name.to_string();
        let t = std::thread::spawn(move || Pipe::connect(&name2).unwrap());
        server.accept().unwrap();
        (server, t.join().unwrap())
    }

    #[test]
    fn second_instance_is_refused() {
        let name = format!(r"\\.\pipe\deskway-test-first-{}", std::process::id());
        let sddl = format!("D:P(A;;GA;;;{})", own_sid().unwrap());
        let _a = Pipe::create(&name, &sddl).unwrap();
        assert!(Pipe::create(&name, &sddl).is_err());
    }

    #[test]
    fn handshake_needs_the_same_key_on_both_sides() {
        let (s, c) = pair(&format!(r"\\.\pipe\deskway-test-hs-{}", std::process::id()));
        let t = std::thread::spawn(move || handshake_client(&c, &[1; 32]).map(|_| c));
        handshake_server(&s, &[1; 32]).unwrap();
        let c = t.join().unwrap().unwrap();
        // Messages cross in both directions at once.
        let t = std::thread::spawn(move || {
            c.send(&("KeyA", true)).unwrap();
            c.recv::<String>().unwrap().len()
        });
        s.send(&"x".repeat(300_000)).unwrap();
        assert_eq!(s.recv::<(String, bool)>().unwrap(), ("KeyA".to_string(), true));
        assert_eq!(t.join().unwrap(), 300_000);
    }

    #[test]
    fn a_stuck_reader_ends_the_connection_instead_of_stalling_the_writer() {
        WRITE_TIMEOUT_MS.store(300, std::sync::atomic::Ordering::Relaxed);
        let (s, c) = pair(&format!(r"\\.\pipe\deskway-test-stuck-{}", std::process::id()));
        let s = Arc::new(s);
        // This end also waits for messages, as the services do; it must not hang either.
        let reader = { let s = s.clone(); std::thread::spawn(move || s.recv::<String>().is_err()) };
        let started = std::time::Instant::now();
        // The other end never reads: the pipe's buffer fills, then the write gives up.
        let big = "x".repeat(4 << 20);
        let first = s.send(&big);
        assert!(first.is_err(), "a write nobody reads ends in an error");
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "and soon: {:?}", started.elapsed());
        assert!(s.send(&"stop").is_err(), "the connection is over, later writes fail at once");
        assert!(reader.join().unwrap(), "the waiting read ends too");
        drop(c);
        WRITE_TIMEOUT_MS.store(10_000, std::sync::atomic::Ordering::Relaxed);
    }

    fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(m: &T) {
        let b = encode(m).unwrap();
        assert_eq!(&decode::<T>(&b[5..]).unwrap(), m);
    }

    #[test]
    fn every_call_and_event_round_trips() {
        let d = DisplayInfo { id: "5f3a9c0e7b12d846".into(), x: -1920, y: 0, w: 1920, h: 1080, primary: false };
        let calls = vec![
            Call::VideoStart { session: "s1".into(), view: crate::video::Constraints { w: 1170, h: 2532, max_w: 3840, max_h: 2160, fps: 30, level: 31 }, display: Some(d.id.clone()) },
            Call::VideoStop { session: "s1".into() },
            Call::Keyframe { session: "s1".into() },
            Call::Bitrate { session: "s1".into(), bps: 16_000_000 },
            Call::NetworkBehind { session: "s1".into(), behind: true },
            Call::ViewConstraints { session: "s1".into(), w: 1170, h: 2532, max_w: 3840, max_h: 2160, fps: 30, level: 31 },
            Call::LocalCursor { session: "s1".into(), on: true },
            Call::ChooseDisplay { session: "s1".into(), id: d.id.clone() },
            Call::Thumbnails { on: false },
            Call::MouseMove { session: "s1".into(), x: 0.25, y: 1.0, seq: 7 },
            Call::MouseMoveBy { session: "s1".into(), dx: -3.5, dy: 2.0, seq: 8 },
            Call::Char { session: "s1".into(), ch: "é".into(), mods: wire::types::Mods { shift: false, ctrl: true, alt: false, meta: false } },
            Call::SyncKeys { session: "s1".into(), locks: wire::types::Locks { caps: true, num: false, scroll: true }, mods: crate::input::no_mods() },
            Call::MouseButton { session: "s1".into(), button: 2, down: true },
            Call::Wheel { session: "s1".into(), dx: -120.5, dy: 3.0 },
            Call::Key { session: "s1".into(), code: "KeyA".into(), down: false },
            Call::Text { session: "s1".into(), text: "héllo 你好 🙂".into() },
            Call::PointerDisplay { session: "s1".into(), display: d.clone() },
            Call::ReleaseAll { session: "s1".into() },
            Call::FileMessage { session: "s1".into(), text: r#"{"t":"put"}"#.into() },
            Call::FileData { session: "s1".into(), data: Bytes((0..=255).collect()) },
            Call::Lock {},
            Call::AutoLock { on: true },
            Call::AutoStart { on: false },
            Call::Language { code: Some("zh-Hant".into()) },
            Call::ClipboardShare { session: "s1".into(), on: true },
            Call::ClipboardAnnounce { session: "s1".into(), gen: 3, formats: vec![crate::clipboard::Format { size: Some(5), ..crate::clipboard::Format::of(FormatKind::Text) }] },
            Call::ClipboardProvide { session: "s1".into(), gen: 3, format: FormatKind::Text, text: Some("hi".into()), html: None },
            Call::ClipboardRead { session: "s1".into(), gen: 3, format: FormatKind::Files, id: "g1".into(), i: Some(1) },
            Call::AiSession { session: Some("a1".into()) },
            Call::Snapshot { id: "q1".into(), display: Some(d.id.clone()), max: 1280 },
            Call::InputDone { session: "s1".into(), id: "q3".into() },
            Call::SyncServerRoot {},
        ];
        // The compiler names any variant added later and missing here.
        let seen: std::collections::BTreeSet<usize> = calls
            .iter()
            .map(|c| match c {
                Call::VideoStart { .. } => 0, Call::VideoStop { .. } => 1, Call::Keyframe { .. } => 2, Call::Bitrate { .. } => 3,
                Call::LocalCursor { .. } => 4, Call::ChooseDisplay { .. } => 5, Call::Thumbnails { .. } => 6,
                Call::MouseMove { .. } => 7, Call::MouseButton { .. } => 8, Call::Wheel { .. } => 9, Call::Key { .. } => 10,
                Call::Text { .. } => 11, Call::PointerDisplay { .. } => 12, Call::ReleaseAll { .. } => 13,
                Call::FileMessage { .. } => 14, Call::FileData { .. } => 15, Call::Lock {} => 16,
                Call::AutoLock { .. } => 17, Call::AutoStart { .. } => 18, Call::Language { .. } => 19,
                Call::MouseMoveBy { .. } => 20, Call::Char { .. } => 21, Call::SyncKeys { .. } => 22,
                Call::ViewConstraints { .. } => 23, Call::ClipboardShare { .. } => 24,
                Call::ClipboardAnnounce { .. } => 25, Call::ClipboardProvide { .. } => 26, Call::ClipboardRead { .. } => 27,
                Call::NetworkBehind { .. } => 28, Call::AiSession { .. } => 29, Call::Snapshot { .. } => 30,
                Call::InputDone { .. } => 31, Call::SyncServerRoot {} => 32,
            })
            .collect();
        assert_eq!(seen.len(), 33);
        calls.iter().for_each(round_trip);

        let events = vec![
            PlatformEvent::Frame(Bytes(vec![0, 0, 0, 1, 0x67, 255])),
            PlatformEvent::Capturing { list: vec![d.clone()], current: d.clone() },
            PlatformEvent::CaptureAvailable(false),
            PlatformEvent::EncodedSize { w: 1170, h: 658, max_bitrate: 1_616_000 },
            PlatformEvent::CursorShape(CursorShape { w: 2, h: 1, hx: -1, hy: 0, rgba: vec![1, 2, 3, 4, 5, 6, 7, 8] }),
            PlatformEvent::CursorPosition { x: 0.5, y: 0.75, visible: true, seq: 9 },
            PlatformEvent::LayoutChanged { name: "fr-FR".into(), labels: [("KeyQ".into(), "a".into())].into(), shift: [("KeyQ".into(), "A".into())].into() },
            PlatformEvent::Thumbnail { display: "1".into(), jpeg: Bytes(vec![0xff, 0xd8, 0xff]) },
            PlatformEvent::DisplaysChanged(Vec::new()),
            PlatformEvent::Desktop(Desk::Winlogon),
            PlatformEvent::FileMessage(r#"{"t":"ready"}"#.into()),
            PlatformEvent::TransferNote(Some(Note { text: "Received 1 file".into(), folder: Some(r"X:\Example\Downloads\Deskway".into()) })),
            PlatformEvent::TransferNote(None),
            PlatformEvent::FileData(Bytes(vec![1, 2, 3])),
            PlatformEvent::ClipboardChanged { formats: vec![crate::clipboard::Format::of(FormatKind::Html)], text: None },
            PlatformEvent::ClipboardRender { gen: 4, format: FormatKind::Image },
            PlatformEvent::ClipboardAnnounced { gen: 4, why: Some(ClipboardError::Io) },
            PlatformEvent::ClipboardData { gen: 4, format: FormatKind::Image, id: "g".into(), name: Some("image.png".into()), text: None, html: None, size: Some(9) },
            PlatformEvent::ClipboardFailed { gen: 4, id: "g".into(), why: ClipboardError::Gone },
            PlatformEvent::Snapshot { id: "q1".into(), display: "1".into(), jpeg: Bytes(vec![0xff, 0xd8]), w: 1280, h: 720 },
            PlatformEvent::SnapshotFailed { id: "q2".into() },
            PlatformEvent::InputDone { id: "q3".into() },
            PlatformEvent::ServerRootSynced { error: Some("certificate store access denied".into()) },
        ];
        let seen: std::collections::BTreeSet<usize> = events
            .iter()
            .map(|e| match e {
                PlatformEvent::Frame(_) => 0, PlatformEvent::Capturing { .. } => 1, PlatformEvent::CaptureAvailable(_) => 2,
                PlatformEvent::CursorShape(_) => 3, PlatformEvent::CursorPosition { .. } => 4, PlatformEvent::Thumbnail { .. } => 5,
                PlatformEvent::DisplaysChanged(_) => 6, PlatformEvent::Desktop(_) => 7, PlatformEvent::FileMessage(_) => 8,
                PlatformEvent::TransferNote(_) => 9, PlatformEvent::LayoutChanged { .. } => 10,
                PlatformEvent::EncodedSize { .. } => 11, PlatformEvent::FileData(_) => 12, PlatformEvent::ClipboardChanged { .. } => 13,
                PlatformEvent::ClipboardRender { .. } => 14, PlatformEvent::ClipboardData { .. } => 15, PlatformEvent::ClipboardFailed { .. } => 16,
                PlatformEvent::ClipboardAnnounced { .. } => 17, PlatformEvent::Snapshot { .. } => 18, PlatformEvent::SnapshotFailed { .. } => 19,
                PlatformEvent::InputDone { .. } => 20, PlatformEvent::ServerRootSynced { .. } => 21,
            })
            .collect();
        assert_eq!(seen.len(), 22);
        events.iter().for_each(round_trip);
    }

    #[test]
    fn bytes_cross_raw() {
        let body = encode(&PlatformEvent::Frame(Bytes(vec![7; 1000]))).unwrap();
        // Variant index, length, then the bytes themselves.
        assert!(body.len() - 5 <= 1000 + 4, "{}", body.len());
        assert!(body.ends_with(&[7; 1000]));
    }

    #[test]
    fn a_broken_peer_cannot_ask_for_a_huge_allocation() {
        // A byte string claiming 4 GB inside a small frame.
        let mut b = encode(&PlatformEvent::Frame(Bytes(vec![1]))).unwrap()[5..].to_vec();
        b.truncate(1);
        b.extend_from_slice(&[0xfc, 0xff, 0xff, 0xff, 0xff]);
        assert!(decode::<PlatformEvent>(&b).is_err());
    }

    /// Measured, not asserted tightly: a 500 KB keyframe should cost little next to its 33 ms.
    #[test]
    fn a_video_frame_costs_little_to_encode() {
        let frame: Vec<u8> = (0..500_000u32).map(|i| (i * 7 + i / 13) as u8).collect();
        let e = PlatformEvent::Frame(Bytes(frame));
        let n = 200;
        let t = std::time::Instant::now();
        let mut len = 0;
        for _ in 0..n {
            len = encode(&e).unwrap().len() - 5;
        }
        let enc = t.elapsed() / n;
        let body = encode(&e).unwrap();
        let t = std::time::Instant::now();
        for _ in 0..n {
            let _: PlatformEvent = decode(&body[5..]).unwrap();
        }
        let dec = t.elapsed() / n;
        println!("500 KB frame: {len} bytes, encode {enc:?}, decode {dec:?}");
        assert!(len <= 500_000 + 16, "a few bytes of framing, nothing more");
        assert!(enc < std::time::Duration::from_millis(5) && dec < std::time::Duration::from_millis(5), "{enc:?} {dec:?}");
    }

    #[test]
    fn a_large_frame_crosses_the_pipe() {
        let (s, c) = pair(&format!(r"\\.\pipe\deskway-test-large-{}", std::process::id()));
        let frame = PlatformEvent::Frame(Bytes((0..8_000_000u32).map(|i| i as u8).collect()));
        let sent = frame.clone();
        let t = std::thread::spawn(move || c.send(&sent).unwrap());
        assert_eq!(s.recv::<PlatformEvent>().unwrap(), frame);
        t.join().unwrap();
    }

    #[test]
    fn impostor_server_learns_nothing() {
        let (s, c) = pair(&format!(r"\\.\pipe\deskway-test-bad-{}", std::process::id()));
        let t = std::thread::spawn(move || handshake_client(&c, &[1; 32]));
        assert!(handshake_server(&s, &[2; 32]).is_err());
        assert!(t.join().unwrap().is_err());
    }
}
