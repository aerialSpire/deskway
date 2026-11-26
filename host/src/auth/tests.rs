use super::*;
use super::offers::{Verified, RESUME_FOR};
use super::pairing::{new_pairing_code, PAIR_TRIES};
use super::webauthn::ES256;
use wire::crypto::{random, sha256};
use wire::types::Role;
use wire::challenge::{ai_v1, connect_v3, endorse_v1, endorse_v2, key_code, normalize_code, pair_v1, paired_v1, policy_v1, remove_v1, rename_v1, resume_v3, vouch_v1};
use p256::ecdsa::signature::{Signer, Verifier};
use p256::ecdsa::SigningKey;
use p256::pkcs8::DecodePublicKey;
use serde_json::json;
use std::time::Instant;

fn endorse_challenge(cred: &str, alg: i64, key: &str, name: &str, ts: i64) -> [u8; 32] {
    sha256(endorse_v1(cred, alg, key, name, ts).as_bytes())
}

fn remove_challenge(cred: &str, ts: i64) -> [u8; 32] {
    sha256(remove_v1(cred, ts).as_bytes())
}

fn policy_challenge(keys: bool, ts: i64) -> [u8; 32] {
    sha256(policy_v1(keys, ts).as_bytes())
}

fn vouch_challenge(host: &str, list: &[Controller]) -> [u8; 32] {
    let signed: Vec<(&str, i64, &str)> = list.iter().map(|c| (c.id.as_str(), c.alg, c.key.as_str())).collect();
    sha256(vouch_v1(host, &signed).as_bytes())
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("deskway-auth-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Plays the browser and its authenticator.
struct Browser {
    key: SigningKey,
    cred: String,
}

impl Browser {
    fn new() -> Browser {
        Browser { key: SigningKey::from_slice(&random::<32>()).unwrap(), cred: URL_SAFE_NO_PAD.encode(random::<16>()) }
    }
    fn spki(&self) -> String {
        STANDARD.encode(self.key.verifying_key().to_public_key_der().unwrap().as_bytes())
    }
    fn pair(&self, host: &str, code: &str) -> Value {
        let code = normalize_code(code);
        let mac = hmac(code.as_bytes(), &pair_v1(host, &self.cred, ES256, &self.spki(), "test"));
        json!({ "cred": self.cred, "alg": ES256, "key": self.spki(), "name": "test", "mac": STANDARD.encode(mac) })
    }
    fn assert(&self, rp: &str, origin: &str, challenge: &[u8], flags: u8, count: u32) -> Value {
        let mut ad = sha256(rp.as_bytes()).to_vec();
        ad.push(flags);
        ad.extend_from_slice(&count.to_be_bytes());
        let cd = json!({ "type": "webauthn.get", "challenge": URL_SAFE_NO_PAD.encode(challenge), "origin": origin }).to_string();
        let mut signed = ad.clone();
        signed.extend_from_slice(&sha256(cd.as_bytes()));
        let sig: p256::ecdsa::Signature = self.key.sign(&signed);
        json!({ "cred": self.cred, "ad": URL_SAFE_NO_PAD.encode(&ad), "cd": URL_SAFE_NO_PAD.encode(cd), "sig": URL_SAFE_NO_PAD.encode(sig.to_der().as_bytes()) })
    }
    /// A connect proof with this device's key over deskway-connect-v3.
    fn offer_auth(&self, session: &str, nonce: &str, sdp: &str, force: bool) -> Value {
        let sig: p256::ecdsa::Signature = self.key.sign(&sha256(connect_v3("H1", session, nonce, force, sdp).as_bytes()));
        json!({ "k": "dk", "nonce": nonce, "cred": self.cred, "sig": URL_SAFE_NO_PAD.encode(sig.to_bytes()) })
    }
}

fn paired(name: &str) -> (Trust, Browser) {
    let d = dir(name);
    let code = Trust::start_pairing(&d).unwrap();
    let mut t = Trust::open(d, "H1", "https://node.example/").unwrap();
    let b = Browser::new();
    let reply = t.pair(&b.pair("H1", &code.to_lowercase())).unwrap();
    let mac = STANDARD.decode(reply.mac.unwrap()).unwrap();
    assert!(hmac_ok(normalize_code(&code).as_bytes(), &paired_v1("H1", &b.cred, &reply.hk), &mac));
    (t, b)
}

#[test]
fn pairing_code_has_80_bits_in_four_groups() {
    let c = new_pairing_code();
    assert_eq!(c.len(), 19);
    assert_eq!(normalize_code(&c).len(), 16);
}

#[test]
fn origin_is_scheme_and_authority() {
    assert_eq!(origin_of("https://192-0-2-1.example.net").unwrap(), ("192-0-2-1.example.net".into(), "https://192-0-2-1.example.net".into()));
    assert_eq!(origin_of("http://127.0.0.1:18091/x").unwrap(), ("127.0.0.1".into(), "http://127.0.0.1:18091".into()));
}

#[test]
fn a_paired_device_opens_a_session_with_the_nonce() {
    let (mut t, b) = paired("ok");
    let a = b.offer_auth("s1", "N1", "v=0 sdp", false);
    let v = t.verify_offer("s1", "N1", "v=0 sdp", false, &a).unwrap();
    assert_eq!(v.controller, b.cred);
    // The proof is bound to the nonce the host issued: another one (a replay against a later
    // nonce) does not verify.
    assert!(t.verify_offer("s1", "N2", "v=0 sdp", false, &a).is_err(), "replay");
    assert!(t.verify_offer("s1", "", "v=0 sdp", false, &a).is_err(), "no nonce issued");
}

#[test]
fn a_rewritten_sdp_or_session_is_refused() {
    let (mut t, b) = paired("sdp");
    let a = b.offer_auth("s1", "N", "v=0 sdp", false);
    assert!(t.verify_offer("s1", "N", "v=0 other fingerprint", false, &a).is_err());
    assert!(t.verify_offer("s2", "N", "v=0 sdp", false, &a).is_err());
}

#[test]
fn unpaired_device_and_unsigned_offer_are_refused() {
    let (mut t, _) = paired("unpaired");
    let stranger = Browser::new();
    assert!(t.verify_offer("s1", "N", "x", false, &stranger.offer_auth("s1", "N", "x", false)).is_err());
    assert!(t.verify_offer("s2", "N", "x", false, &json!({})).is_err());
}

#[test]
fn wrong_code_uses_up_the_window() {
    let d = dir("wrong");
    Trust::start_pairing(&d).unwrap();
    let mut t = Trust::open(d, "H1", "https://node.example").unwrap();
    let b = Browser::new();
    for _ in 0..PAIR_TRIES {
        assert!(t.pair(&b.pair("H1", "AAAA-AAAA-AAAA-AAAA")).is_err());
    }
    assert!(t.pairing_url().is_none());
    assert!(t.controllers().is_empty());
}

/// A message as it goes on the wire.
fn json_of(m: &impl serde::Serialize) -> Value {
    serde_json::to_value(m).unwrap()
}

fn resume_auth(r: &wire::control::Control, session: &str, nonce: &str, sdp: &str) -> Value {
    let r = json_of(r);
    let key = STANDARD.decode(r["k"].as_str().unwrap()).unwrap();
    json!({ "k": "rs", "id": r["id"], "nonce": nonce, "mac": STANDARD.encode(hmac(&key, &resume_v3("H1", session, nonce, false, sdp))) })
}

#[test]
fn resume_token_reconnects_and_dies_with_its_controller() {
    let (mut t, b) = paired("resume");
    let v = t.verify_offer("s1", "N", "x", false, &b.offer_auth("s1", "N", "x", false)).unwrap();
    let r = t.issue_resume(&v);
    let nonce = URL_SAFE_NO_PAD.encode(random::<8>());
    let resumed = t.verify_offer("s2", &nonce, "y", false, &resume_auth(&r, "s2", &nonce, "y")).unwrap();
    assert_eq!((resumed.controller, resumed.page), (v.controller, v.page));
    t.remove(&b.cred).unwrap();
    assert!(t.verify_offer("s3", "N3", "y", false, &resume_auth(&r, "s3", "N3", "y")).is_err());
}

#[test]
fn revoked_resume_tokens_no_longer_reconnect() {
    let (mut t, b) = paired("revoke");
    let v = t.verify_offer("s1", "N", "x", false, &b.offer_auth("s1", "N", "x", false)).unwrap();
    let r = t.issue_resume(&v);
    let other = t.issue_resume(&Verified { controller: "someone-else".into(), name: "x".into(), role: Role::Browser, since: Instant::now(), page: String::new() });
    let open = t.issue_resume(&Verified { controller: String::new(), name: "x".into(), role: Role::Browser, since: Instant::now(), page: String::new() });
    t.revoke_resume(&b.cred);
    assert!(t.verify_offer("s2", "N2", "y", false, &resume_auth(&r, "s2", "N2", "y")).is_err());
    t.verify_offer("s3", "N3", "y", false, &resume_auth(&other, "s3", "N3", "y")).expect("other controllers keep theirs");
    // A session without a device (checks off) names nobody: revoking for it takes nothing.
    t.revoke_resume("");
    t.verify_offer("s4", "N4", "y", false, &resume_auth(&open, "s4", "N4", "y")).expect("an empty controller revokes nothing");
}

#[test]
fn force_is_part_of_the_signature() {
    let (mut t, b) = paired("force");
    // Signed without force: a server that adds force breaks the signature.
    let a = b.offer_auth("s1", "N", "x", false);
    assert!(t.verify_offer("s1", "N", "x", true, &a).is_err());
    t.verify_offer("s1", "N", "x", true, &b.offer_auth("s1", "N", "x", true)).unwrap();
}

#[test]
fn resume_tokens_do_not_outlive_the_passkey_check() {
    let (mut t, b) = paired("chain");
    let v = t.verify_offer("s1", "N", "x", false, &b.offer_auth("s1", "N", "x", false)).unwrap();
    let first = t.issue_resume(&v);
    let renewed = Verified { controller: v.controller.clone(), name: v.name.clone(), role: v.role, since: v.since, page: v.page.clone() };
    t.issue_resume(&renewed);
    let expiries: Vec<Instant> = t.resume.values().map(|r| r.expires).collect();
    assert!(expiries.iter().all(|e| *e == v.since + RESUME_FOR), "{first:?}");
}

impl Browser {
    fn device(&self, name: &str) -> Controller {
        Controller { id: self.cred.clone(), alg: ES256, key: self.spki(), name: name.into(), added: 0, sign_count: 0, used: 0, role: Role::Browser }
    }
    fn sign(&self, challenge: &[u8]) -> Value {
        self.assert("node.example", "https://node.example", challenge, 0x05, 0)
    }
    fn endorse(&self, other: &Browser, ts: i64) -> Value {
        let d = other.device("iPad");
        let mut e = self.sign(&endorse_challenge(&d.id, d.alg, &d.key, &d.name, ts));
        e["op"] = json!("add");
        e["cred"] = json!(d.id);
        e["alg"] = json!(d.alg);
        e["key"] = json!(d.key);
        e["name"] = json!(d.name);
        e["ts"] = json!(ts);
        e["by"] = json!(self.cred);
        e
    }
    fn remove(&self, other: &Browser, ts: i64) -> Value {
        let mut e = self.sign(&remove_challenge(&other.cred, ts));
        e["op"] = json!("remove");
        e["cred"] = json!(other.cred);
        e["ts"] = json!(ts);
        e["by"] = json!(self.cred);
        e
    }
}

#[test]
fn log_adds_devices_only_through_trusted_signers() {
    let (mut t, phone) = paired("log");
    let ipad = Browser::new();
    let stranger = Browser::new();
    let evil = Browser::new();
    // Signed by the paired phone: trusted. Signed by a stranger: ignored.
    t.set_log(&json!([phone.endorse(&ipad, 1), stranger.endorse(&evil, 2)]), &Value::Null);
    assert!(t.controllers().iter().any(|c| c.id == ipad.cred));
    assert!(!t.controllers().iter().any(|c| c.id == evil.cred));
    t.verify_offer("s1", "N", "x", false, &ipad.offer_auth("s1", "N", "x", false)).unwrap();
    // The iPad can now bring in more, and a signed removal takes it out again.
    let later = unix_now() as i64 + 5;
    t.set_log(&json!([phone.endorse(&ipad, 1), phone.remove(&ipad, later)]), &Value::Null);
    assert!(!t.controllers().iter().any(|c| c.id == ipad.cred));
    // A removal of the phone itself, signed by the phone, forgets it at this computer for good.
    t.set_log(&json!([phone.remove(&phone, later)]), &Value::Null);
    assert!(t.controllers().is_empty());
    // Added here again afterwards, the old removal no longer applies to it.
    let code = Trust::start_pairing(&t.dir).unwrap();
    let earlier = unix_now() as i64 - 100;
    t.set_log(&json!([phone.remove(&phone, earlier)]), &Value::Null);
    t.pair(&phone.pair("H1", &code)).unwrap();
    assert_eq!(t.controllers().len(), 1);
    t.set_log(&json!([phone.remove(&phone, earlier)]), &Value::Null);
    assert_eq!(t.controllers().len(), 1);
    // The server hiding the log cannot take away a device added here.
    t.set_log(&json!([]), &Value::Null);
    assert_eq!(t.controllers().len(), 1);
}

impl DeviceKey {
    /// `by` names `self`.
    fn rename_entry(&self, by: &DeviceKey, name: &str, ts: i64) -> Value {
        json!({ "op": "rename", "cred": self.cred, "name": name, "ts": ts, "by": by.cred, "sig": by.sig(&sha256(rename_v1(&self.cred, name, ts).as_bytes())) })
    }
}

#[test]
fn a_trusted_device_renames_a_trusted_device() {
    let d = dir("rename");
    let mut t = Trust::open(d, "H1", "https://node.example").unwrap();
    let phone = DeviceKey::new();
    let laptop = DeviceKey::new();
    let stranger = DeviceKey::new();
    let name = |t: &Trust, k: &DeviceKey| t.controllers().iter().find(|c| c.id == k.cred).map(|c| c.name.clone());
    let added = [phone.add_entry(None, 1), laptop.add_entry(Some(&phone), 2)];
    // Signed by the phone: the laptop has its new name, and the newest rename counts.
    t.set_log(&json!([added[0], added[1], laptop.rename_entry(&phone, "新加坡办公室 PC", 3), laptop.rename_entry(&laptop, "Office PC", 4)]), &Value::Null);
    assert_eq!(name(&t, &laptop).as_deref(), Some("Office PC"));
    // Signed by a stranger, or with the name changed after signing: ignored.
    let mut forged = phone.rename_entry(&phone, "mine", 5);
    forged["name"] = json!("not mine");
    t.set_log(&json!([added[0], added[1], laptop.rename_entry(&stranger, "evil", 5), forged]), &Value::Null);
    assert_eq!(name(&t, &laptop).as_deref(), Some("dev"));
    assert_eq!(name(&t, &phone).as_deref(), Some("dev"));
    // A rename makes nobody trusted.
    t.set_log(&json!([added[0], stranger.rename_entry(&phone, "in", 5)]), &Value::Null);
    assert!(name(&t, &stranger).is_none());
}

#[test]
fn forged_log_entries_are_ignored() {
    let (mut t, phone) = paired("forged");
    let ipad = Browser::new();
    let mut e = phone.endorse(&ipad, 1);
    e["key"] = json!(Browser::new().spki()); // swap the key the phone signed for
    t.set_log(&json!([e]), &Value::Null);
    assert_eq!(t.controllers().len(), 1);
}

#[test]
fn asking_at_the_computer_and_vouching() {
    let d = dir("ask");
    let mut t = Trust::open(d, "H1", "https://node.example").unwrap();
    let laptop = Browser::new();
    let phone = Browser::new();
    // The laptop vouches for the phone, which the account already has, and for the account's
    // AI client, which is left out: an AI client comes in only through a browser's endorsement.
    let claude = Browser::new();
    let list = vec![phone.device("iPhone"), claude.device("Claude Code")];
    let mut v = laptop.sign(&vouch_challenge("H1", &list));
    v["list"] = json!([{ "cred": phone.cred, "alg": ES256, "key": phone.spki(), "name": "iPhone" }, { "cred": claude.cred, "alg": ES256, "key": claude.spki(), "name": "Claude Code", "role": "ai" }]);
    let m = json!({ "cred": laptop.cred, "alg": ES256, "key": laptop.spki(), "name": "MacBook", "vouch": v });
    // An AI client cannot ask at the computer.
    let mut as_ai = m.clone();
    as_ai["role"] = json!("ai");
    assert!(t.ask("s0", &as_ai, "Alex").is_err());
    let a = t.ask("s1", &m, "Alex").unwrap();
    assert_eq!(a.code, key_code(Role::Browser, &laptop.spki()));
    assert!(t.controllers().is_empty(), "nothing is trusted before the person answers");
    let reply = t.answer("s1", true).unwrap().unwrap();
    assert_eq!(json_of(&reply)["type"], "paired");
    assert_eq!(t.controllers().len(), 2);
    // A refused or unknown request changes nothing.
    let other = Browser::new();
    t.ask("s2", &json!({ "cred": other.cred, "alg": ES256, "key": other.spki(), "name": "x" }), "Alex").unwrap();
    assert_eq!(json_of(&t.answer("s2", false).unwrap().unwrap())["reason"], "denied");
    assert!(t.answer("s3", true).unwrap().is_none());
    assert_eq!(t.controllers().len(), 2);
}

#[test]
fn at_most_50_devices_and_when_each_was_last_used() {
    let d = dir("full");
    let mut t = Trust::open(d.clone(), "H1", "https://node.example").unwrap();
    let ask = |b: &Browser| json!({ "cred": b.cred, "alg": ES256, "key": b.spki(), "name": "x" });
    let first = Browser::new();
    for i in 0..MAX_CONTROLLERS {
        let b = if i == 0 { Browser { key: first.key.clone(), cred: first.cred.clone() } } else { Browser::new() };
        t.ask(&format!("s{i}"), &ask(&b), "Alex").unwrap();
        assert_eq!(json_of(&t.answer(&format!("s{i}"), true).unwrap().unwrap())["type"], "paired");
    }
    assert_eq!(t.controllers().len(), MAX_CONTROLLERS);
    // One more is refused as full, before anyone is asked; one already here may ask again.
    let err = t.ask("over", &ask(&Browser::new()), "Alex").unwrap_err();
    assert!(err.is::<DevicesFull>(), "{err:#}");
    t.ask("again", &ask(&first), "Alex").unwrap();
    // Used: noted, and kept across a restart.
    assert_eq!(t.controllers()[0].used, 0, "not used yet");
    t.mark_used(&first.cred).unwrap();
    let t = Trust::open(d, "H1", "https://node.example").unwrap();
    assert!(t.controllers().iter().find(|c| c.id == first.cred).unwrap().used > 0);
}

#[test]
fn a_vouch_signed_by_someone_else_is_refused() {
    let d = dir("vouch");
    let mut t = Trust::open(d, "H1", "https://node.example").unwrap();
    let laptop = Browser::new();
    let evil = Browser::new();
    let list = vec![evil.device("evil")];
    let mut v = evil.sign(&vouch_challenge("H1", &list));
    v["list"] = json!([{ "cred": evil.cred, "alg": ES256, "key": evil.spki(), "name": "evil" }]);
    let m = json!({ "cred": laptop.cred, "alg": ES256, "key": laptop.spki(), "name": "MacBook", "vouch": v });
    assert!(t.ask("s1", &m, "Alex").is_err());
}

/// A browser-made device key: plain ECDSA, no authenticator.
struct DeviceKey {
    key: SigningKey,
    cred: String,
}

impl DeviceKey {
    fn new() -> DeviceKey {
        DeviceKey { key: SigningKey::from_slice(&random::<32>()).unwrap(), cred: URL_SAFE_NO_PAD.encode(random::<16>()) }
    }
    fn spki(&self) -> String {
        STANDARD.encode(self.key.verifying_key().to_public_key_der().unwrap().as_bytes())
    }
    fn sig(&self, challenge: &[u8]) -> String {
        let s: p256::ecdsa::Signature = self.key.sign(challenge);
        URL_SAFE_NO_PAD.encode(s.to_bytes())
    }
    fn add_entry(&self, by: Option<&DeviceKey>, ts: i64) -> Value {
        let mut e = json!({ "op": "add", "cred": self.cred, "alg": ES256, "key": self.spki(), "name": "dev", "ts": ts });
        if let Some(b) = by {
            e["by"] = json!(b.cred);
            e["sig"] = json!(b.sig(&endorse_challenge(&self.cred, ES256, &self.spki(), "dev", ts)));
        }
        e
    }
    /// An AI client let in by `by`: its role signed (endorse-v2).
    fn ai_entry(&self, by: &DeviceKey, ts: i64) -> Value {
        let sig = by.sig(&sha256(endorse_v2(&self.cred, ES256, &self.spki(), "claude", "ai", ts).as_bytes()));
        json!({ "op": "add", "cred": self.cred, "alg": ES256, "key": self.spki(), "name": "claude", "ts": ts, "by": by.cred, "sig": sig, "role": "ai" })
    }
    /// This device's AI switch for computer `host`.
    fn switch(&self, host: &str, ai: bool, admin: bool, ts: i64) -> Value {
        json!({ "host": host, "ai": ai, "admin": admin, "ts": ts, "by": self.cred, "sig": self.sig(&sha256(ai_v1(host, ai, admin, ts).as_bytes())) })
    }
    fn offer(&self, session: &str, sdp: &str) -> Value {
        json!({ "k": "dk", "nonce": "N", "cred": self.cred, "sig": self.sig(&sha256(connect_v3("H1", session, "N", false, sdp).as_bytes())) })
    }
}

#[test]
fn a_new_computer_adopts_the_account_devices_once() {
    let d = dir("adopt");
    let mut t = Trust::open(d.clone(), "H1", "https://node.example").unwrap();
    // Nothing yet: nothing adopted, and no marker, so the first device later still counts.
    t.set_log(&json!([]), &Value::Null);
    assert!(t.controllers().is_empty());
    let phone = DeviceKey::new();
    let laptop = DeviceKey::new();
    t.set_log(&json!([phone.add_entry(None, 1), laptop.add_entry(Some(&phone), 2)]), &Value::Null);
    assert_eq!(t.controllers().len(), 2);
    t.verify_offer("s1", "N", "x", false, &phone.offer("s1", "x")).unwrap();
    t.verify_offer("s2", "N", "x", false, &laptop.offer("s2", "x")).unwrap();
    // After that, an unsigned addition from the server is ignored.
    let evil = DeviceKey::new();
    t.set_log(&json!([phone.add_entry(None, 1), laptop.add_entry(Some(&phone), 2), evil.add_entry(None, 3)]), &Value::Null);
    assert!(!t.controllers().iter().any(|c| c.id == evil.cred));
    // Everything removed at the computer: it does not adopt again.
    t.remove_all().unwrap();
    t.set_log(&json!([evil.add_entry(None, 3)]), &Value::Null);
    assert!(t.controllers().is_empty());
}

#[test]
fn device_checks_can_be_turned_off_only_by_a_trusted_device() {
    let d = dir("policy");
    let mut t = Trust::open(d, "H1", "https://node.example").unwrap();
    let phone = DeviceKey::new();
    let stranger = DeviceKey::new();
    let off = |by: &DeviceKey| json!({ "op": "policy", "keys": false, "ts": 5, "by": by.cred, "sig": by.sig(&policy_challenge(false, 5)) });
    t.set_log(&json!([phone.add_entry(None, 1), off(&stranger)]), &Value::Null);
    assert!(t.keys_required, "a stranger cannot turn checks off");
    assert!(t.verify_offer("s1", "N", "x", false, &json!({})).is_err());
    t.set_log(&json!([phone.add_entry(None, 1), off(&phone)]), &Value::Null);
    assert!(!t.keys_required);
    t.verify_offer("s2", "N", "x", false, &json!({})).unwrap();
    let on = json!({ "op": "policy", "keys": true, "ts": 6, "by": phone.cred, "sig": phone.sig(&policy_challenge(true, 6)) });
    t.set_log(&json!([phone.add_entry(None, 1), off(&phone), on]), &Value::Null);
    assert!(t.keys_required);
}

#[test]
fn a_device_removed_here_stays_out_until_added_here_again() {
    let d = dir("removehere");
    let mut t = Trust::open(d.clone(), "H1", "https://node.example").unwrap();
    let phone = DeviceKey::new();
    let laptop = DeviceKey::new();
    let log = json!([phone.add_entry(None, 1), laptop.add_entry(Some(&phone), 2)]);
    t.set_log(&log, &Value::Null);
    assert_eq!(t.controllers().len(), 2);
    // The laptop came in through the log; removing it here keeps it out despite the log.
    assert_eq!(t.remove_here(&[laptop.cred.clone()]).unwrap(), 1);
    t.set_log(&log, &Value::Null);
    assert_eq!(t.controllers().len(), 1);
    assert!(t.verify_offer("s1", "N", "x", false, &laptop.offer("s1", "x")).is_err());
    // Survives a restart.
    let mut t = Trust::open(d, "H1", "https://node.example").unwrap();
    t.set_log(&log, &Value::Null);
    assert_eq!(t.controllers().len(), 1);
    // Everything: the count is zero at once.
    assert_eq!(t.remove_all().unwrap(), 1);
    assert!(t.controllers().is_empty());
    t.set_log(&log, &Value::Null);
    assert!(t.controllers().is_empty());
}

#[test]
fn answer_signature_verifies_with_published_key() {
    let (t, _) = paired("answer");
    let s = (t.answer_signer("s1"))("offer", "answer");
    let der = STANDARD.decode(&s.hk).unwrap();
    let k = p256::ecdsa::VerifyingKey::from_public_key_der(&der).unwrap();
    let sig = p256::ecdsa::Signature::from_slice(&STANDARD.decode(&s.sig).unwrap()).unwrap();
    k.verify(wire::challenge::answer_v1("H1", "s1", "offer", "answer").as_bytes(), &sig).unwrap();
}

/// pair-v1 and paired-v1 are checked through pairing itself: a host with the golden vector's
/// key and code accepts the vector's MAC and answers with the other.
#[test]
fn pairing_accepts_and_answers_the_golden_macs() {
    use wire::contract::{input as s, vector, vectors};
    let all = vectors();
    let pair = vector(&all, "deskway-pair-v1");
    let paired = vector(&all, "deskway-paired-v1");
    let d = dir("golden-pair");
    let scalar: Vec<u8> = (0..32).map(|i| u8::from_str_radix(&all["keys"]["host_key"]["scalar"].as_str().unwrap()[i * 2..i * 2 + 2], 16).unwrap()).collect();
    std::fs::write(d.join("host-key"), scalar).unwrap();
    let window = pairing::Pairing { code: pair["pairing_code"].as_str().unwrap().into(), expires: u64::MAX / 2, tries: 5 };
    std::fs::write(d.join("pairing.json"), serde_json::to_vec(&window).unwrap()).unwrap();
    let mut t = Trust::open(d, s(pair, "host"), "https://node.example").unwrap();
    assert_eq!(t.host_public(), all["keys"]["host_key"]["spki"].as_str().unwrap());
    let m = json!({ "cred": s(pair, "cred"), "alg": pair["inputs"]["alg"], "key": s(pair, "key"), "name": s(pair, "name"), "mac": pair["mac"] });
    let reply = t.pair(&m).expect("the golden pair-v1 MAC is accepted");
    assert_eq!(reply.hk, paired["inputs"]["hostKey"].as_str().unwrap());
    assert_eq!(reply.mac.as_deref(), paired["mac"].as_str(), "paired-v1");
}

/// Offers are verified with the golden resume-v3 string: a MAC over the vector's string (with
/// the token's key) is accepted for the vector's session and nonce.
#[test]
fn offers_resume_with_the_golden_format() {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use wire::contract::{input as s, mac_b64, vector, vectors};
    let all = vectors();
    let v = vector(&all, "deskway-resume-v3");
    let mut t = Trust::open(dir("golden-resume"), s(v, "host"), "https://node.example").unwrap();
    let token = t.issue_resume(&Verified { controller: "c".into(), name: "n".into(), role: Role::Browser, since: Instant::now(), page: String::new() });
    let wire::control::Control::Resume { id, k } = token else { panic!("a resume token is a resume message") };
    let key = STANDARD.decode(k).unwrap();
    let auth = json!({ "k": "rs", "id": id, "nonce": s(v, "nonce"), "mac": mac_b64(&key, v["string"].as_str().unwrap()) });
    t.verify_offer(s(v, "session"), s(v, "nonce"), s(v, "sdp"), v["inputs"]["force"].as_bool().unwrap(), &auth).expect("resume-v3 as in the vector");
}

#[test]
fn ai_clients_come_in_only_through_a_browser_and_sign_for_nobody() {
    let d = dir("ai-roles");
    let mut t = Trust::open(d, "H1", "https://node.example").unwrap();
    let phone = DeviceKey::new();
    let claude = DeviceKey::new();
    let laptop = DeviceKey::new();
    // A new computer adopts the account's browsers, not its AI client.
    let mut listed = claude.add_entry(None, 2);
    listed["role"] = json!("ai");
    t.set_log(&json!([phone.add_entry(None, 1), listed]), &Value::Null);
    assert_eq!(t.controllers().iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), vec![phone.cred.as_str()]);
    let entry = claude.ai_entry(&phone, 3);
    let mut stripped = entry.clone();
    stripped.as_object_mut().unwrap().remove("role");
    let mut as_v1 = claude.add_entry(Some(&phone), 3);
    as_v1["role"] = json!("ai");
    let by_ai = |e: &Value| {
        let mut e = e.clone();
        e["by"] = json!(claude.cred);
        e
    };
    // (log after the phone's own entry, the AI client trusted, the laptop trusted)
    let cases = [
        ("a browser lets the AI client in with its role signed", vec![entry.clone()], true, false),
        ("the server strips the role: the signature no longer covers it", vec![stripped], false, false),
        ("the server adds the role to a browser's endorsement", vec![as_v1], false, false),
        ("the AI client lets a laptop in: it signs for nobody", vec![entry.clone(), laptop.add_entry(Some(&claude), 4)], true, false),
        (
            "nor turns device checks off",
            vec![entry.clone(), by_ai(&json!({ "op": "policy", "keys": false, "ts": 5, "sig": claude.sig(&policy_challenge(false, 5)) }))],
            true,
            false,
        ),
    ];
    for (what, entries, ai_trusted, laptop_trusted) in cases {
        let log: Vec<Value> = std::iter::once(phone.add_entry(None, 1)).chain(entries).collect();
        t.set_log(&json!(log), &Value::Null);
        let ai = t.controllers().iter().find(|c| c.id == claude.cred).map(|c| c.role);
        assert_eq!(ai, ai_trusted.then_some(Role::Ai), "{what}");
        assert_eq!(t.controllers().iter().any(|c| c.id == laptop.cred), laptop_trusted, "{what}");
        assert!(t.keys_required, "{what}");
    }
    let v = t.verify_offer("s1", "N", "x", false, &claude.offer("s1", "x")).unwrap();
    assert_eq!(v.role, Role::Ai, "an offer says which seat it is for");
    assert_eq!(t.verify_offer("s2", "N", "x", false, &phone.offer("s2", "x")).unwrap().role, Role::Browser);
}

#[test]
fn local_stop_applies_to_the_saved_switch() {
    for (name, kept, want) in [
        ("missing", None, Ok((false, AiState::Off))),
        ("off", Some(r#"{"switch":{"ts":5,"ai":false,"admin":false},"stopped":null}"#), Ok((false, AiState::Off))),
        ("on", Some(r#"{"switch":{"ts":5,"ai":true,"admin":true},"stopped":4}"#), Ok((true, AiState::Stopped))),
        ("stopped", Some(r#"{"switch":{"ts":5,"ai":true,"admin":false},"stopped":5}"#), Ok((false, AiState::Stopped))),
        ("damaged", Some("{"), Err(())),
    ] {
        let d = dir(&format!("saved-ai-{name}"));
        let path = d.join("ai.json");
        if let Some(kept) = kept {
            std::fs::write(&path, kept).unwrap();
        }
        let result = ai::stop_saved_ai(&d).and_then(|changed| Ok((changed, ai::saved_ai_state(&d)?)));
        assert_eq!(result.map_err(|_| ()), want, "{name}");
        if !matches!(want, Ok((true, _))) {
            assert_eq!(std::fs::read_to_string(path).ok().as_deref(), kept, "{name}");
        }
    }
}

#[test]
fn the_ai_switch_is_the_newest_a_trusted_browser_signed_for_this_computer() {
    let d = dir("ai-switch");
    let mut t = Trust::open(d.clone(), "H1", "https://node.example").unwrap();
    let phone = DeviceKey::new();
    let stranger = DeviceKey::new();
    let claude = DeviceKey::new();
    let log = json!([phone.add_entry(None, 1), claude.ai_entry(&phone, 2)]);
    let on = AiState::On { admin: false };
    // (what, the switch the server sends, the state after it)
    let cases = [
        ("never set", Value::Null, AiState::Off),
        ("signed by a device this computer does not trust", stranger.switch("H1", true, false, 10), AiState::Off),
        ("signed by the AI client itself", claude.switch("H1", true, false, 10), AiState::Off),
        ("for another computer", phone.switch("H2", true, false, 10), AiState::Off),
        ("a trusted browser turns it on", phone.switch("H1", true, false, 10), on),
        ("an older switch (the server rolling back) changes nothing", phone.switch("H1", false, false, 9), on),
        ("the same one again", phone.switch("H1", true, false, 10), on),
        ("with administrator rights", phone.switch("H1", true, true, 11), AiState::On { admin: true }),
    ];
    for (what, switch, state) in cases {
        t.set_log(&log, &switch);
        assert_eq!(t.ai_state(), state, "{what}");
    }
    // Stop, pressed here: it holds over the same switch and a restart, until a newer one.
    t.stop_ai();
    t.set_log(&log, &phone.switch("H1", true, true, 11));
    assert_eq!(t.ai_state(), AiState::Stopped);
    let mut t = Trust::open(d, "H1", "https://node.example").unwrap();
    t.set_log(&log, &Value::Null);
    assert_eq!(t.ai_state(), AiState::Stopped, "a restart keeps the switch and the stop");
    t.set_log(&log, &phone.switch("H1", true, false, 12));
    assert_eq!(t.ai_state(), on, "a newer switch opens it again");
    t.set_log(&log, &phone.switch("H1", false, false, 13));
    assert_eq!(t.ai_state(), AiState::Off);
    t.stop_ai();
    t.set_log(&log, &phone.switch("H1", true, false, 14));
    assert_eq!(t.ai_state(), on, "stop while off holds nothing back");
}

#[test]
fn a_switch_from_a_device_not_trusted_yet_is_reported_and_taken_once_it_is() {
    let mut t = Trust::open(dir("ai-refused"), "H1", "https://node.example").unwrap();
    let phone = DeviceKey::new();
    let laptop = DeviceKey::new();
    let log = json!([phone.add_entry(None, 1)]);
    t.set_log(&log, &laptop.switch("H1", true, false, 10));
    assert_eq!((t.ai_state(), t.ai_refused()), (AiState::Off, Some(10)), "refused, and said so");
    // The phone it trusts endorses the laptop: the switch it refused takes, with no new one.
    t.set_log(&json!([phone.add_entry(None, 1), laptop.add_entry(Some(&phone), 2)]), &Value::Null);
    assert_eq!((t.ai_state(), t.ai_refused()), (AiState::On { admin: false }, None));
}
