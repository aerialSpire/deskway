//! Signing in as an AI client (docs/architecture.md, "Identity"), without the account
//! password: while a browser of the account takes AI sign-ins (it opened them, on Add an AI client),
//! a browser approves this device after comparing six digits (code-v2, made here from the key and
//! the role, never taken from the server). Approved, it signs in again with its key alone. The server keeps this device's
//! login to what an AI client needs; which computers it may use is each computer's AI switch.

use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use deskway_server_connection::{Address, Connection};
use serde_json::{json, Value};
use wire::crypto::{hex, random, sha256};
use wire::challenge::key_code;
use wire::types::Role;

use crate::keys::DeviceKey;
use crate::store::{Account, SignInLost};

fn agent(server: &Connection) -> Result<ureq::Agent> {
    server.http(ureq::builder().user_agent(&format!("deskway/{}", env!("CARGO_PKG_VERSION"))).timeout(Duration::from_secs(30)))
}

/// The cookie `name`'s value among a response's Set-Cookie headers.
fn cookie(r: &ureq::Response, name: &str) -> Option<String> {
    r.all("set-cookie").into_iter().find_map(|c| c.split(';').next()?.trim().strip_prefix(&format!("{name}=")).map(str::to_string)).filter(|v| !v.is_empty())
}

/// A request with this device's cookies; any answer is returned, a failure to reach the server
/// is an error.
fn call(a: &mut Account, req: ureq::Request, body: Option<Value>) -> Result<ureq::Response> {
    let req = req.set("Cookie", &a.cookies());
    let r = match body {
        Some(b) => req.send_json(b),
        None => req.call(),
    };
    let r = match r {
        Ok(r) | Err(ureq::Error::Status(_, r)) => r,
        Err(e) => return Err(e).context("cannot reach the server"),
    };
    if let Some(b) = cookie(&r, "deskway_browser") {
        a.browser = b;
    }
    if let Some(s) = cookie(&r, "deskway_session") {
        a.session = s;
    }
    Ok(r)
}

/// Asks to sign in as `a.user`, signed with this device's key: an approved AI client gets its
/// session (204); a new one, while the account takes AI sign-ins, waits for approval (202),
/// unless this is only a renewal, which never asks anyone.
fn ask(a: &mut Account, server: &Connection, http: &ureq::Agent, key: &DeviceKey, renew: bool) -> Result<ureq::Response> {
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or_default();
    let sig = key.sign(&wire::crypto::sha256(wire::challenge::ai_login_v1(&a.user, ts).as_bytes()));
    call(a, http.post(&server.url("/api/login/ai")), Some(json!({ "user": a.user, "ts": ts, "sig": sig, "renew": renew })))
}

/// A GET of the account's server as this device, signed in; its JSON answer.
pub fn get_json(a: &mut Account, path: &str) -> Result<Value> {
    let server = a.connection()?;
    let r = call(a, agent(&server)?.get(&server.url(path)), None)?;
    match r.status() {
        200 => r.into_json().context("the server's answer"),
        s => bail!("the server answered {s}"),
    }
}

/// A file from the account's server, whole.
pub fn download(a: &mut Account, path: &str) -> Result<Vec<u8>> {
    let server = a.connection()?;
    let r = call(a, agent(&server)?.get(&server.url(path)), None)?;
    if r.status() != 200 {
        bail!("the server answered {}", r.status());
    }
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut r.into_reader(), &mut bytes).context("download")?;
    Ok(bytes)
}

/// Signs in again with this device's key, as an approved AI client does whenever its session
/// has ended (the proxy, on being refused).
pub fn renew(a: &mut Account) -> Result<()> {
    let saved = Account::load()?;
    a.check_sign_in(&saved)?;
    if *a != saved {
        *a = saved;
        return Ok(());
    }
    let key = DeviceKey::load_or_create()?;
    let server = a.connection()?;
    match ask(a, &server, &agent(&server)?, &key, true)?.status() {
        204 => a.save_renewal(&saved),
        401 => Err(SignInLost("signing in again was refused (401): run `deskway login` again").into()),
        s => bail!("signing in again was refused ({s}): run `deskway login` again"),
    }
}

fn device(key: &DeviceKey, name: &str) -> Value {
    json!({ "cred": key.cred, "alg": -7, "key": key.spki(), "name": name, "role": "ai" })
}

/// Prepares a fresh profile for the server's existing browser endorsement. The
/// public key and cookie hash identify the pending device; the cookie stays here.
pub fn prepare(server: &str, user: &str, name: &str) -> Result<Value> {
    let address = Address::parse(server)?;
    let server_root = address.fetch_root()?;
    let key = DeviceKey::load_or_create()?;
    let browser = hex(&random::<32>());
    let id = hex(&sha256(browser.as_bytes()));
    Account { server: address.saved(), server_root, user: user.into(), name: name.into(), browser, ..Account::default() }.save()?;
    Ok(json!({ "id": id, "device": device(&key, name) }))
}

/// Completes a prepared profile through the approved AI client's signed login.
pub fn finish_prepared() -> Result<()> {
    let mut a = Account::load()?;
    if a.server.is_empty() || a.user.is_empty() || a.browser.is_empty() {
        bail!("no prepared sign-in");
    }
    let key = DeviceKey::load_or_create()?;
    let server = a.connection()?;
    match ask(&mut a, &server, &agent(&server)?, &key, true)?.status() {
        204 => a.save(),
        status => bail!("the prepared sign-in was refused ({status})"),
    }
}

/// Signs in to `server` as `user`, without the account password; `shown` gets the six digits to
/// compare and what to do.
pub fn login(server: &str, user: &str, name: &str, shown: impl Fn(&str)) -> Result<Account> {
    let address = Address::parse(server)?;
    let account = Account::load().unwrap_or_default();
    let key = DeviceKey::load_or_create()?;
    let account = sign_in(account, &address, user, name, &key, shown)?;
    account.save()?;
    Ok(account)
}

fn sign_in(mut a: Account, address: &Address, user: &str, name: &str, key: &DeviceKey, shown: impl Fn(&str)) -> Result<Account> {
    let saved_address = address.saved();
    if a.server != saved_address || a.user != user {
        let server_root = if a.server == saved_address { a.server_root } else { None };
        a = Account { server: saved_address, server_root, user: user.to_string(), ..Account::default() };
    }
    a.name = name.to_string();
    let server = match address.connection(a.server_root.as_deref()) {
        Err(_) if address.is_pinned() => {
            a.server_root = address.fetch_root()?;
            address.connection(a.server_root.as_deref())?
        }
        result => result?,
    };
    let http = agent(&server)?;
    let r = ask(&mut a, &server, &http, key, false)?;
    match r.status() {
        204 => return Ok(a),
        202 => {}
        403 => bail!("{} is not taking AI sign-ins now. On a browser signed in to this account, open My devices, then Add an AI client, copy the prompt there, and sign in within 10 minutes", a.server),
        401 => bail!("{} did not accept this device's key: remove it in My devices, then sign in again", a.server),
        429 => bail!("too many tries from here: wait a while"),
        s => bail!("the server answered {s}"),
    }
    let mine = key_code(Role::Ai, &key.spki());
    let r = call(&mut a, http.post(&server.url("/api/login/device")), Some(device(key, name)))?;
    let theirs = r.into_json::<Value>().ok().and_then(|v| v["code"].as_str().map(str::to_string)).unwrap_or_default();
    if theirs != mine {
        bail!("the server shows other digits than this device ({theirs} / {mine}): stopped, it may not be your server");
    }
    shown(&mine);
    let until = Instant::now() + Duration::from_secs(10 * 60);
    while Instant::now() < until {
        std::thread::sleep(Duration::from_secs(2));
        match call(&mut a, http.get(&server.url("/api/login/status")), None)?.status() {
            204 => return Ok(a),
            202 => {}
            _ => bail!("the request was denied or has expired"),
        }
    }
    bail!("nobody approved this device in 10 minutes")
}

#[cfg(test)]
#[path = "../../crates/server/src/test_tls.rs"]
mod test_tls;

#[cfg(test)]
mod tests {
    use super::*;
    use test_tls::{listen, TestTls};

    #[test]
    fn signs_in_with_a_pinned_address_and_keeps_its_root_in_the_profile() {
        let listener = listen();
        let tls = TestTls::new();
        let saved = tls.address(&listener);
        let address = Address::parse(&saved).unwrap();
        let expected_root = address.pin_root(&tls.root).unwrap();
        let server = std::thread::spawn(move || {
            tls.reply(&listener, "GET /local/ca.crt", "200 OK", &[], &tls.root);
            tls.reply(&listener, "POST /api/login/ai", "204 No Content",
                &["Set-Cookie: deskway_session=first"], b"");
            tls.reply(&listener, "POST /api/login/ai", "204 No Content",
                &["Set-Cookie: deskway_session=again"], b"");
            for _ in 0..2 {
                tls.reply(&listener, "GET /local/ca.crt", "200 OK", &[], &tls.root);
                tls.reply(&listener, "POST /api/login/ai", "204 No Content",
                    &["Set-Cookie: deskway_session=recovered"], b"");
            }
        });
        let key = DeviceKey::generate().unwrap();
        let no_prompt = |_: &str| panic!("this key is already approved");
        let account = sign_in(Account::default(), &address, "me", "Test", &key, no_prompt).unwrap();
        assert_eq!(account.server, saved);
        assert_eq!(account.server_root.as_deref(), Some(expected_root.as_str()));
        assert_eq!(account.session, "first");
        let saved = serde_json::to_vec(&account).unwrap();
        let profile = serde_json::from_slice(&saved).unwrap();
        let account = sign_in(profile, &address, "me", "Test", &key, no_prompt).unwrap();
        assert_eq!(account.session, "again");
        for server_root in [None, Some("invalid PEM".into())] {
            let profile = Account { server_root, ..account.clone() };
            let recovered = sign_in(profile, &address, "me", "Test", &key, no_prompt).unwrap();
            assert_eq!(recovered.session, "recovered");
            assert_eq!(recovered.server_root.as_deref(), Some(expected_root.as_str()));
        }
        server.join().unwrap();
    }

    #[test]
    fn forget_cleans_up_a_profile_with_an_unusable_root() {
        const CHILD: &str = "DESKWAY_TEST_FORGET_PROFILE";
        if std::env::var_os(CHILD).is_some() {
            logout(true).unwrap();
            assert_eq!(Account::load().unwrap(), Account::default());
            assert!(!crate::store::dir().unwrap().join("device.json").exists());
            return;
        }

        let dir = std::env::temp_dir().join(format!("deskway-forget-{}", hex(&random::<16>())));
        crate::os::create_private_dir(&dir).unwrap();
        let account = Account {
            server: format!("https://hub.local#ca={}", "ab".repeat(32)),
            session: "signed in".into(),
            ..Account::default()
        };
        std::fs::write(dir.join("account.json"), serde_json::to_vec(&account).unwrap()).unwrap();
        std::fs::write(dir.join("device.json"), b"unreadable key").unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "account::tests::forget_cleans_up_a_profile_with_an_unusable_root"])
            .env(CHILD, "1")
            .env("DESKWAY_CLI_HOME", &dir)
            .output().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stdout));
    }
}

/// Signs out here: the server forgets the session; the key stays, so a new sign-in is the same
/// device (`forget` drops it too).
pub fn logout(forget: bool) -> Result<()> {
    crate::proxy_client::stop_proxy();
    let mut a = Account::clear()?;
    let remote = (|| -> Result<()> {
        if a.signed_in() {
            let server = a.connection()?;
            let _ = call(&mut a, agent(&server)?.post(&server.url("/api/logout")), None);
        }
        Ok(())
    })();
    if forget {
        DeviceKey::forget()?;
        Account::default().save()?;
    } else {
        remote?;
    }
    Ok(())
}
