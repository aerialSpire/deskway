use serde_json::{json, Value};

use crate::local_server::{Reply, Request, State};

#[derive(Default)]
pub struct Panel {
    pub state: Option<State>,
    pub link_allowed: Option<bool>,
    serial: u64,
    result: Value,
}

impl Panel {
    pub fn view(&self) -> Value {
        json!({ "state": self.state, "result": self.result, "link_allowed": self.link_allowed })
    }

    pub fn replied(&mut self, action: Request, result: Result<Reply, String>) {
        self.serial += 1;
        self.result = match result {
            Ok(Reply::Usage {
                computers,
                browsers,
            }) => {
                json!({ "id": self.serial, "action": action, "computers": computers, "browsers": browsers })
            }
            Ok(_) => json!({ "id": self.serial, "action": action }),
            Err(error) => json!({ "id": self.serial, "action": action, "error": error }),
        };
    }
}
