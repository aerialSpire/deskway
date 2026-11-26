//! Signing in as an AI client (docs/architecture.md, "Identity"), without the account
//! password: while a browser of the account takes AI sign-ins (it opened them, on Add an AI client),
//! a browser approves this device after comparing six digits (code-v2, made here from the key and
//! the role, never taken from the server). Approved, it signs in again with its key alone. The server keeps this device's
//! login to what an AI client needs; which computers it may use is each computer's AI switch.

use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use wire::crypto::{hex, random, sha256};
use wire::challenge::key_code;
use wire::types::Role;

use crate::keys::DeviceKey;
use crate::store::{Account, SignInLost};

fn agent() -> Result<ureq::Agent> {
    let tls = native_tls::TlsConnector::new().context("TLS")?;
    Ok(ureq::AgentBuilder::new().tls_connector(std::sync::Arc::new(tls)).user_agent(&format!("deskway/{}", env!("CARGO_PKG_VERSION"))).timeout(Duration::from_secs(30)).build())
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
fn ask(a: &mut Account, http: &ureq::Agent, key: &DeviceKey, renew: bool) -> Result<ureq::Response> {
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or_default();
    let sig = key.sign(&wire::crypto::sha256(wire::challenge::ai_login_v1(&a.user, ts).as_bytes()));
    let url = format!("{}/api/login/ai", a.server);
    call(a, http.post(&url), Some(json!({ "user": a.user, "ts": ts, "sig": sig, "renew": renew })))
}

/// A GET of the account's server as this device, signed in; its JSON answer.
pub fn get_json(a: &mut Account, path: &str) -> Result<Value> {
    let url = format!("{}{path}", a.server);
    let r = call(a, agent()?.get(&url), None)?;
    match r.status() {
        200 => r.into_json().context("the server's answer"),
        s => bail!("the server answered {s}"),
    }
}

/// A file from the account's server, whole.
pub fn download(a: &mut Account, path: &str) -> Result<Vec<u8>> {
    let url = format!("{}{path}", a.server);
    let r = call(a, agent()?.get(&url), None)?;
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
    match ask(a, &agent()?, &key, true)?.status() {
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
    let key = DeviceKey::load_or_create()?;
    let browser = hex(&random::<32>());
    let id = hex(&sha256(browser.as_bytes()));
    Account { server: server.into(), user: user.into(), name: name.into(), browser, ..Account::default() }.save()?;
    Ok(json!({ "id": id, "device": device(&key, name) }))
}

/// Completes a prepared profile through the approved AI client's signed login.
pub fn finish_prepared() -> Result<()> {
    let mut a = Account::load()?;
    if a.server.is_empty() || a.user.is_empty() || a.browser.is_empty() {
        bail!("no prepared sign-in");
    }
    let key = DeviceKey::load_or_create()?;
    match ask(&mut a, &agent()?, &key, true)?.status() {
        204 => a.save(),
        status => bail!("the prepared sign-in was refused ({status})"),
    }
}

/// Signs in to `server` as `user`, without the account password; `shown` gets the six digits to
/// compare and what to do.
pub fn login(server: &str, user: &str, name: &str, shown: impl Fn(&str)) -> Result<Account> {
    let server = server.trim_end_matches('/').to_string();
    let mut a = Account::load().unwrap_or_default();
    if a.server != server || a.user != user {
        a = Account { server: server.clone(), user: user.to_string(), ..Account::default() };
    }
    a.name = name.to_string();
    let key = DeviceKey::load_or_create()?;
    let http = agent()?;
    let r = ask(&mut a, &http, &key, false)?;
    match r.status() {
        204 => {
            a.save()?;
            return Ok(a);
        }
        202 => {}
        403 => bail!("{server} is not taking AI sign-ins now. On a browser signed in to this account, open My devices, then Add an AI client, copy the prompt there, and sign in within 10 minutes"),
        401 => bail!("{server} did not accept this device's key: remove it in My devices, then sign in again"),
        429 => bail!("too many tries from here: wait a while"),
        s => bail!("the server answered {s}"),
    }
    let mine = key_code(Role::Ai, &key.spki());
    let r = call(&mut a, http.post(&format!("{server}/api/login/device")), Some(device(&key, name)))?;
    let theirs = r.into_json::<Value>().ok().and_then(|v| v["code"].as_str().map(str::to_string)).unwrap_or_default();
    if theirs != mine {
        bail!("the server shows other digits than this device ({theirs} / {mine}): stopped, it may not be your server");
    }
    shown(&mine);
    let until = Instant::now() + Duration::from_secs(10 * 60);
    while Instant::now() < until {
        std::thread::sleep(Duration::from_secs(2));
        match call(&mut a, http.get(&format!("{server}/api/login/status")), None)?.status() {
            204 => {
                a.save()?;
                return Ok(a);
            }
            202 => {}
            _ => bail!("the request was denied or has expired"),
        }
    }
    bail!("nobody approved this device in 10 minutes")
}

/// Signs out here: the server forgets the session; the key stays, so a new sign-in is the same
/// device (`forget` drops it too).
pub fn logout(forget: bool) -> Result<()> {
    crate::proxy_client::stop_proxy();
    let mut a = Account::clear()?;
    if a.signed_in() {
        let url = format!("{}/api/logout", a.server);
        let _ = call(&mut a, agent()?.post(&url), None);
    }
    if forget {
        DeviceKey::forget()?;
        Account::default().save()?;
    }
    Ok(())
}
