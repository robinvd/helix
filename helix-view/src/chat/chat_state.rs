use helix_core::regex::Regex;
use serde::Serialize;
use tokio::sync::mpsc::Receiver;

use super::{client::ChatClient, context::Context};

#[derive(Serialize, Clone, Debug)]
pub struct Message {
    pub content: String,
    pub role: String,
}

pub struct InProgressState {
    pub channel: Receiver<String>,
    pub ticks: usize,
}

#[derive(Default)]
pub struct ChatState {
    pub history: Vec<Message>,
    pub context: Vec<Context>,
    pub in_progress: Option<InProgressState>,
    pub client: ChatClient,
}

impl ChatState {
    pub fn fetch_inprogress(&mut self) {
        if let Some(ref mut recv) = self.in_progress {
            let last = self.history.last_mut().unwrap();
            while let Ok(res) = recv.channel.try_recv() {
                log::info!("new copilot text: {res:?}");
                last.content.push_str(&res);
                recv.ticks += 1;
            }
        }
    }
    pub fn get_last_code_changes(&self) -> Vec<(String, usize, usize, String)> {
        let last_msg = self.history.last().unwrap().clone();
        log::info!("finding code block: {:?}", last_msg.content);
        // [file:test.py](test.py) line:1-5\n\n```python\ndef test():\n    \"\"\"\n    A simple test function that initializes a variable `x` to 1.\n    \"\"\"\n    x = 1\n    pass\n```
        let code_regex =
            Regex::new(r"(?ms)^\[file:([^\]]+)\]\([^)]+\) line:(\d+)-(\d+)\n+```\w+\n(.*)\n```")
                .unwrap();

        let mut changes = Vec::new();
        if let Some(m) = code_regex.captures(&last_msg.content) {
            let (_, [filename, line_start, line_end, code]) = m.extract();
            let line_start = line_start.parse::<usize>().unwrap().saturating_sub(1);
            let line_end = line_end.parse::<usize>().unwrap().saturating_sub(1);
            changes.push((filename.to_owned(), line_start, line_end, code.to_owned()));
        }
        changes
    }
}
