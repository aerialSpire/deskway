use serde_json::{json, Value};

use crate::local_server::{Reply, Request, State};

#[derive(Default)]
pub struct Panel {
    pub state: Option<State>,
    serial: u64,
    result: Value,
}

impl Panel {
    pub fn view(&self) -> Value {
        json!({ "state": self.state, "result": self.result })
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

pub fn draw_badge(pixels: &mut [u8], badge: Option<bool>, template: bool) {
    let Some(ok) = badge else { return };
    let color = if template {
        [0, 0, 0, 255]
    } else if ok {
        [29, 151, 91, 255]
    } else {
        [224, 151, 30, 255]
    };
    for y in 18_i32..32 {
        for x in 18_i32..32 {
            let radius = (x - 25).pow(2) + (y - 25).pow(2);
            let start = ((y * 32 + x) * 4) as usize;
            if radius <= 25 {
                pixels[start..start + 4].copy_from_slice(&color);
            } else if radius <= 49 {
                pixels[start..start + 4].fill(0);
            }
        }
    }
}
