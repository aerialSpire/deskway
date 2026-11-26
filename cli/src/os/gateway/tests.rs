use super::*;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let name = wire::crypto::hex(&wire::crypto::random::<8>());
        let path = std::env::temp_dir().join(format!("dw-{name}"));
        crate::os::create_private_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn refuses_unsafe_socket_paths() {
    const PARENT_ERROR: &str = "Open the Unix socket's parent directory.";
    const OCCUPIED_ERROR: &str = "The Unix socket path already exists.";
    let dir = Directory::new();
    let path = dir.0.join("mcp.sock");
    let refusal = |path: &Path, expected| {
        assert_eq!(HttpServer::unix(path).err().unwrap().to_string(), expected);
    };
    refusal(&dir.0.join("missing/mcp.sock"), PARENT_ERROR);
    for mode in [0o710, 0o740, 0o720, 0o701, 0o704, 0o702] {
        crate::os::set_unix_mode(&dir.0, mode).unwrap();
        refusal(&path, "The Unix socket's parent must be a directory owned by this user, with no group or other access.");
    }
    crate::os::set_unix_mode(&dir.0, 0o700).unwrap();
    let file = dir.0.join("file");
    fs::write(&file, b"keep").unwrap();
    refusal(&file, OCCUPIED_ERROR);
    refusal(&file.join("mcp.sock"), PARENT_ERROR);
    symlink(&file, &path).unwrap();
    refusal(&path, OCCUPIED_ERROR);
    assert_eq!(fs::read(&file).unwrap(), b"keep");
    assert_eq!(fs::read_link(&path).unwrap(), file);
}

#[test]
fn socket_cleanup_uses_the_original_parent() {
    let dir = Directory::new();
    let first = Directory::new();
    let second = Directory::new();
    let parent = dir.0.join("alias");
    symlink(&first.0, &parent).unwrap();
    let path = parent.join("mcp.sock");
    let original = HttpServer::unix(&path).unwrap();
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    fs::remove_file(&parent).unwrap();
    symlink(&second.0, &parent).unwrap();
    let replacement = HttpServer::unix(&path).unwrap();
    drop(original);
    assert!(!first.0.join("mcp.sock").exists());
    assert!(UnixStream::connect(&path).is_ok());
    drop(replacement);
    assert!(!path.exists());
}

#[cfg(target_os = "linux")]
mod lifetime {
    use super::*;
    use std::io::{BufRead, BufReader, Read};
    use std::net::TcpListener;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::Instant;

    const ROLE: &str = "DESKWAY_TEST_OWNED_GATEWAY_ROLE";

    fn process(role: &str) -> Command {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["--exact", "os::gateway::tests::lifetime::process_fixture", "--nocapture"])
            .env(ROLE, role).stdin(Stdio::piped()).stdout(Stdio::null());
        // The test harness creates its own threads before entering the gateway fixture.
        unsafe { cmd.pre_exec(|| block_shutdown_signals().map(|_| ())) };
        cmd
    }

    #[test]
    fn process_fixture() {
        match std::env::var(ROLE).as_deref() {
            Ok("supervisor") => {
                let mut gateway = process("gateway").spawn().unwrap();
                let _keep_open = gateway.stdin.take();
                assert!(gateway.wait().unwrap().success());
            }
            Ok("gateway") => {
                let path = crate::store::dir().unwrap().join("mcp.sock");
                if let Err(e) = crate::gateway::run(&["--listen".into(), format!("unix:{}", path.display()), "--own-proxy".into()]) {
                    eprintln!("{e:#}");
                    std::process::exit(1);
                }
            }
            Ok("logout") => crate::account::logout(false).unwrap(),
            _ => {}
        }
    }

    #[test]
    fn owned_proxy_stops_with_its_gateway() {
        let dir = Directory::new();
        let path = dir.0.join("mcp.sock");
        // A stand-in signaling peer holds the real proxy in a handshake. Its stream must
        // close even while the proxy's worker is blocked.
        let signaling = signed_in(&dir);
        signaling.set_nonblocking(true).unwrap();

        for (role, signal) in [
            ("gateway", None), ("gateway", Some(libc::SIGTERM)),
            ("gateway", Some(libc::SIGKILL)), ("supervisor", Some(libc::SIGKILL)),
        ] {
            let mut child = process(role).env("DESKWAY_CLI_HOME", &dir.0).stderr(Stdio::piped()).spawn().unwrap();
            let (ready, waiting) = mpsc::channel();
            let stderr = child.stderr.take().unwrap();
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if line.starts_with("Gateway listening at ") {
                        let _ = ready.send(line);
                    }
                }
            });
            assert_eq!(waiting.recv_timeout(Duration::from_secs(5)).unwrap(), format!("Gateway listening at unix:{}", path.display()));
            let mut upstream = wait_for(|| signaling.accept().ok().map(|(s, _)| s));
            assert!(!dir.0.join("proxy.json").exists());
            match signal {
                Some(signal) => assert_eq!(unsafe { libc::kill(child.id() as libc::pid_t, signal) }, 0),
                None => drop(child.stdin.take()),
            }
            upstream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let result = upstream.read_to_end(&mut Vec::new());
            assert!(result.is_ok() || result.unwrap_err().kind() == io::ErrorKind::ConnectionReset);
            let status = wait_for(|| child.try_wait().unwrap());
            assert_eq!(status.success(), signal != Some(libc::SIGKILL));
            if role == "gateway" && signal == Some(libc::SIGKILL) {
                assert!(path.exists());
                fs::remove_file(&path).unwrap();
            } else {
                assert!(!path.exists());
            }
        }
    }

    #[test]
    fn owned_gateway_exits_when_its_profile_loses_sign_in() {
        use tiny_http::{Header, Response};

        for logout_during_renewal in [false, true] {
            let dir = Directory::new();
            let upstream = HttpServer::new(signed_in(&dir)).unwrap();
            let request = || upstream.server().recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
            let mut gateway = process("gateway").env("DESKWAY_CLI_HOME", &dir.0).stderr(Stdio::piped()).spawn().unwrap();

            let signaling = request();
            assert_eq!(signaling.url(), "/ws/client");
            signaling.respond(Response::empty(401)).unwrap();
            let renewal = request();
            assert_eq!(renewal.url(), "/api/login/ai");

            let expected = if logout_during_renewal {
                let mut logout = process("logout").env("DESKWAY_CLI_HOME", &dir.0).spawn().unwrap();
                let sign_out = request();
                assert_eq!(sign_out.url(), "/api/logout");
                assert!(!crate::store::Account::load_from(&dir.0).unwrap().signed_in());
                let cookie = Header::from_bytes("Set-Cookie", "deskway_session=renewed").unwrap();
                renewal.respond(Response::empty(204).with_header(cookie)).unwrap();
                sign_out.respond(Response::empty(204)).unwrap();
                assert!(wait_for(|| logout.try_wait().unwrap()).success());
                "the profile was signed out"
            } else {
                renewal.respond(Response::empty(401)).unwrap();
                "cannot renew the profile sign-in: signing in again was refused (401)"
            };

            assert_eq!(wait_for(|| gateway.try_wait().unwrap()).code(), Some(1));
            let output = gateway.wait_with_output().unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains(expected), "{stderr}");
            assert!(!dir.0.join("mcp.sock").exists());
            let saved = crate::store::Account::load_from(&dir.0).unwrap();
            assert_eq!(saved.session, if logout_during_renewal { "" } else { "test" });
        }
    }

    fn signed_in(dir: &Directory) -> TcpListener {
        let signaling = TcpListener::bind("127.0.0.1:0").unwrap();
        let account = crate::store::Account {
            server: format!("http://{}", signaling.local_addr().unwrap()),
            session: "test".into(), ..Default::default()
        };
        fs::write(dir.0.join("account.json"), serde_json::to_vec(&account).unwrap()).unwrap();
        signaling
    }

    fn wait_for<T>(mut read: impl FnMut() -> Option<T>) -> T {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(value) = read() {
                return value;
            }
            assert!(Instant::now() < until, "timed out waiting for the test process");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
