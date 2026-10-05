use std::cell::RefCell;

pub struct Probe {
    enabled: bool,
    first: RefCell<Option<Record>>,
}

pub struct Record {
    stage: &'static str,
    pid: Option<u32>,
    uid: Option<u32>,
    state: Option<char>,
    errno: Option<i32>,
    code: Option<i32>,
}

impl Record {
    pub fn render(&self) -> String {
        format!(
            "process_probe stage={} pid={} uid={} state={} errno={} code={}\n",
            self.stage,
            number(self.pid),
            number(self.uid),
            self.state.unwrap_or('-'),
            number(self.errno),
            number(self.code),
        )
    }
}

fn number(value: Option<impl std::fmt::Display>) -> String {
    value.map_or_else(|| "-".into(), |value| value.to_string())
}

impl Probe {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            first: RefCell::new(None),
        }
    }

    pub fn take(&self) -> Option<Record> {
        self.first.borrow_mut().take()
    }

    pub fn failed(&self, stage: &'static str, pid: Option<u32>, errno: Option<i32>) {
        if self.enabled {
            self.first.borrow_mut().get_or_insert(Record {
                stage,
                pid,
                uid: None,
                state: None,
                errno,
                code: None,
            });
        }
    }

    pub fn row(&self, pid: Option<u32>, uid: Option<u32>, state: Option<char>) {
        if let Some(first) = self.first.borrow_mut().as_mut() {
            first.pid = pid;
            first.uid = uid;
            first.state = state.filter(char::is_ascii_alphabetic);
        }
    }

    pub fn status(&self, code: Option<i32>) {
        self.failed("ps_status", None, None);
        if let Some(first) = self.first.borrow_mut().as_mut() {
            first.code = code;
        }
    }

    pub fn parse_failure(&self, line: &str) {
        let mut fields = line.split_whitespace();
        let pid = fields.next().and_then(|value| value.parse().ok());
        let uid = fields.next().and_then(|value| value.parse().ok());
        let state = fields.next().and_then(|value| value.chars().next());
        self.failed("row_parser", pid, None);
        self.row(pid, uid, state);
    }
}
