//! What the person at this computer asks of the host from its panel, and the settings the host
//! carries out. One owner for both: a setting is saved in the config first, then applied (the
//! keep-awake request, the language, and the signaling host through its `Control` queue).
//!
//! The process that runs the host holds the [`Settings`]: the macOS app calls it directly, the
//! Windows panel sends its [`PanelRequest`]s over the UI pipe to the network service, which
//! calls it there. The controlling browser's clipboard switch and an administrator's edit of the
//! config file arrive here too.

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::i18n::{self, t};
use crate::platform::Platform;
use crate::signal::Control;

/// A request from the panel.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum PanelRequest {
    /// Lock the computer when a session ends.
    AutoLock(bool),
    /// The shared clipboard: the person at the computer decides, as the controller can.
    ClipShare(bool),
    /// The language the host shows text in: one of the catalog codes.
    Lang(String),
    /// Keep the computer from sleeping (`power`).
    PreventSleep(bool),
    /// Start with the computer or not (Windows: the services' start type, set by the agent).
    AutoStart(bool),
    /// End the session in progress.
    Disconnect,
    /// Show a QR code that adds a phone or computer.
    StartPairing,
    StopPairing,
    /// The person's answer to a device asking to be let in.
    AnswerAsk(String, bool),
    /// Removes devices here: these ids, or all of them.
    RemoveDevices(Option<Vec<String>>),
    /// Stop AI control here, until a browser of the account allows it again.
    StopAi,
}

/// The settings as last applied. Only what differs from them is applied, once.
#[derive(Clone, PartialEq, Debug)]
struct Applied {
    auto_lock: bool,
    clip_share: bool,
    lang: Option<String>,
    prevent_sleep: bool,
}

impl Applied {
    fn of(c: &Config) -> Applied {
        Applied { auto_lock: c.auto_lock, clip_share: c.clip_share, lang: c.lang.clone(), prevent_sleep: c.prevent_sleep }
    }
}

pub struct Settings {
    /// The folder of the config these settings are saved in.
    dir: PathBuf,
    applied: Mutex<Applied>,
    platform: Arc<dyn Platform>,
    control: Sender<Control>,
}

impl Settings {
    /// Starts from the saved settings and applies what `signal::run` does not start from
    /// itself: the platform's language (replayed to every Windows agent that connects later)
    /// and keep-awake. The error, if any, is keep-awake's: the settings are in place anyway.
    pub fn start(dir: PathBuf, cfg: &Config, platform: Arc<dyn Platform>, control: Sender<Control>) -> (Settings, Result<()>) {
        let applied = Applied::of(cfg);
        platform.language(applied.lang.clone());
        let awake = keep_awake(applied.prevent_sleep);
        (Settings { dir, applied: Mutex::new(applied), platform, control }, awake)
    }

    /// Carries out a request: a setting is saved, then applied; anything else goes to the host.
    pub fn handle(&self, r: PanelRequest) -> Result<()> {
        let control = match r {
            PanelRequest::AutoLock(on) => return self.change(|c| c.auto_lock = on),
            PanelRequest::ClipShare(on) => return self.change(|c| c.clip_share = on),
            PanelRequest::PreventSleep(on) => return self.change(|c| c.prevent_sleep = on),
            PanelRequest::Lang(code) => {
                let Some(lang) = i18n::Lang::parse(&code) else { return Ok(()) };
                return self.change(|c| c.lang = Some(lang.code().to_string()));
            }
            PanelRequest::AutoStart(on) => Control::SetAutoStart(on),
            PanelRequest::Disconnect => Control::Disconnect,
            PanelRequest::StartPairing => Control::StartPairing,
            PanelRequest::StopPairing => Control::StopPairing,
            PanelRequest::AnswerAsk(session, allow) => Control::AnswerAsk(session, allow),
            PanelRequest::RemoveDevices(ids) => Control::RemoveDevices(ids),
            PanelRequest::StopAi => Control::StopAi,
        };
        let _ = self.control.send(control);
        Ok(())
    }

    /// What the server says of the account (`account`), kept for the panel while offline.
    /// Written only when it changed.
    pub fn set_account(&self, account: crate::config::Account) -> Result<()> {
        if self.load()?.account.as_ref() == Some(&account) {
            return Ok(());
        }
        Config::update_in(&self.dir, |c| c.account = Some(account)).with_context(|| t("err.save_settings"))?;
        Ok(())
    }

    /// The settings as saved now.
    pub fn load(&self) -> Result<Config> {
        Config::load_from(&self.dir).context("cannot read settings")
    }

    /// The config file changed (the installer's language).
    #[cfg(any(windows, test))]
    pub fn reload(&self) -> Result<()> {
        self.apply(Applied::of(&self.load()?))
    }

    fn change(&self, f: impl FnOnce(&mut Config)) -> Result<()> {
        let c = Config::update_in(&self.dir, f).with_context(|| t("err.save_settings"))?;
        self.apply(Applied::of(&c))
    }

    fn apply(&self, now: Applied) -> Result<()> {
        let mut was = self.applied.lock().unwrap();
        if *was == now {
            return Ok(());
        }
        let mut awake = Ok(());
        if was.auto_lock != now.auto_lock {
            let _ = self.control.send(Control::SetAutoLock(now.auto_lock));
        }
        if was.clip_share != now.clip_share {
            let _ = self.control.send(Control::SetClipShare(now.clip_share));
        }
        if was.lang != now.lang {
            i18n::set(now.lang.as_deref());
            self.platform.language(now.lang.clone());
        }
        if was.prevent_sleep != now.prevent_sleep {
            awake = keep_awake(now.prevent_sleep);
        }
        *was = now;
        // The panel hears of it now, not at the next change of status.
        let _ = self.control.send(Control::Report);
        awake
    }
}

fn keep_awake(on: bool) -> Result<()> {
    crate::power::prevent_sleep(on).with_context(|| t("err.keep_awake"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{Call, Forward};
    use std::sync::mpsc::{channel, Receiver};

    #[derive(Default)]
    struct Calls(Mutex<Vec<Call>>);
    impl Forward for Calls {
        fn forward(&self, call: Call) {
            self.0.lock().unwrap().push(call);
        }
    }

    fn settings(name: &str) -> (Settings, Arc<Calls>, Receiver<Control>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("deskway-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("host.json"), r#"{"server":"https://s.example","id":"H1","token":"t","name":""}"#).unwrap();
        let calls = Arc::new(Calls::default());
        let (tx, rx) = channel();
        let (s, awake) = Settings::start(dir.clone(), &Config::load_from(&dir).unwrap(), calls.clone(), tx);
        awake.unwrap();
        (s, calls, rx, dir)
    }

    #[test]
    fn a_setting_is_saved_then_carried_out_once() {
        let (s, _, rx, dir) = settings("apply");
        s.handle(PanelRequest::ClipShare(true)).unwrap();
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), [Control::SetClipShare(true), Control::Report]);
        assert!(Config::load_from(&dir).unwrap().clip_share, "saved first");
        s.handle(PanelRequest::ClipShare(true)).unwrap();
        assert!(rx.try_recv().is_err(), "nothing changed, nothing carried out");
        // Requests that are no setting go to the host as they are.
        s.handle(PanelRequest::AnswerAsk("s1".into(), true)).unwrap();
        assert_eq!(rx.try_recv().unwrap(), Control::AnswerAsk("s1".into(), true));
    }

    #[test]
    fn the_language_reaches_the_platform_and_an_unknown_one_is_no_choice() {
        let (s, calls, rx, dir) = settings("lang");
        calls.0.lock().unwrap().clear();
        s.handle(PanelRequest::Lang("EN".into())).unwrap();
        assert_eq!(*calls.0.lock().unwrap(), [Call::Language { code: Some("en".into()) }]);
        assert_eq!(Config::load_from(&dir).unwrap().lang.as_deref(), Some("en"), "saved as the catalog's code");
        s.handle(PanelRequest::Lang("tlh".into())).unwrap();
        assert_eq!(calls.0.lock().unwrap().len(), 1);
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), [Control::Report]);
    }

    #[test]
    fn an_edit_of_the_file_is_carried_out_on_reload() {
        let (s, _, rx, dir) = settings("reload");
        Config::update_in(&dir, |c| c.auto_lock = true).unwrap();
        s.reload().unwrap();
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), [Control::SetAutoLock(true), Control::Report]);
        std::fs::remove_file(dir.join("host.json")).unwrap();
        assert!(s.reload().is_err());
        assert!(s.handle(PanelRequest::ClipShare(true)).is_err(), "nothing to save into");
    }
}
