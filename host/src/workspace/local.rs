use super::{apps, elements, files, procs, Answer, Code, Op, WorkError, Workspace};

const WAIT_MOST: u64 = 600_000;

/// The workspace of this process, which is the person's.
#[derive(Default)]
pub struct Local {
    procs: procs::Table,
}

impl Local {
    /// One whose programs are numbered from `first` (the admin worker's, `procs::ADMIN_FIRST`).
    #[cfg(windows)]
    pub fn numbered_from(first: u64) -> Local {
        Local { procs: procs::Table::numbered_from(first) }
    }
}

impl Workspace for Local {
    fn call(&self, op: Op) -> Answer {
        match op {
            Op::Run { command, cwd, wait_ms, elevated } => self.procs.run(&command, cwd.as_deref(), wait_ms.min(WAIT_MOST), elevated),
            Op::ProcStart { command, cwd, cols, rows, elevated } => self.procs.start(command.as_deref(), cwd.as_deref(), cols, rows, elevated),
            Op::ProcWrite { id, text, keys } => self.procs.write(id, text.as_deref(), &keys),
            Op::ProcRead { id, from, wait_ms } => self.procs.read(id, from, wait_ms),
            Op::ProcKill { id } => self.procs.kill(id),
            Op::ProcList => Ok(self.procs.list()),
            Op::FsList { path } => files::list(&path),
            Op::FsStat { path } => files::stat(&path),
            Op::FsRead { path, offset, length } => files::read(&path, offset, length),
            Op::FsWrite { path, text, base64, append } => files::write(&path, text, base64, append),
            Op::FsHash { path } => files::hash(&path),
            Op::FsMove { from, to } => files::rename(&from, &to),
            Op::FsDelete { path, recursive } => files::delete(&path, recursive),
            Op::ClipboardGet => Ok(serde_json::json!({ "text": crate::platform::clipboard_text() })),
            Op::ClipboardSet { text } => {
                if crate::platform::set_clipboard_text(&text) {
                    Ok(serde_json::json!({ "done": "set" }))
                } else {
                    Err(WorkError::new(Code::Busy, "another program holds the clipboard", "Try again in a moment."))
                }
            }
            Op::Windows => apps::windows(),
            Op::WindowAct { id, action, x, y, w, h } => apps::window_act(id, action, x, y, w, h),
            Op::AppList => apps::apps(),
            Op::AppOpen { name, args } => apps::open(&name, &args),
            Op::AppQuit { name, pid, force } => apps::quit(name.as_deref(), pid, force),
            Op::UiTree { window, depth, max_nodes } => elements::tree(window, depth, max_nodes),
            Op::UiAct { window, element, action, value } => elements::act(window, &element, action, value.as_deref()),
        }
    }
}
