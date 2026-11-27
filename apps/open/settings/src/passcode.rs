//! Settings › Passcode: choose, change or turn off the passcode that locks the
//! phone. The Open Runtime keeps and checks it ("passcode" in /dev/violet/sys,
//! runtime/open/power.rs); this page only collects the digits.
use violet::keypad::{self, PadKey};

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum Step {
    /// The page's two buttons; no pad.
    #[default]
    Overview,
    /// Typing the passcode in use, before changing or removing it.
    Current,
    New,
    Confirm,
}

#[derive(Clone, PartialEq, Default)]
pub struct Passcode {
    /// A passcode is in force.
    pub set: bool,
    pub step: Step,
    removing: bool,
    pub typed: String,
    current: String,
    first: String,
    pub note: String,
    /// The note reports success.
    pub good: bool,
}

fn explain(answer: &str) -> String {
    match answer.strip_prefix("error wait ").and_then(|w| w.trim().parse::<u64>().ok()) {
        Some(1) => "Try again in 1 second".into(),
        Some(s) if s <= 90 => format!("Try again in {s} seconds"),
        Some(s) => format!("Try again in {} minutes", s.div_ceil(60)),
        None if answer == "error wrong" => "Wrong passcode".into(),
        None => "Couldn’t save the passcode".into(),
    }
}

impl Passcode {
    pub fn new(set: bool) -> Passcode {
        Passcode { set, ..Passcode::default() }
    }

    /// Start choosing a passcode (or changing it), or turning it off.
    pub fn begin(&mut self, removing: bool) {
        *self = Passcode { set: self.set, removing, step: if self.set { Step::Current } else { Step::New }, ..Passcode::default() };
    }

    /// Back to the two buttons, with what happened.
    fn finish(&mut self, set: bool, note: &str) {
        *self = Passcode { set, note: note.into(), good: true, ..Passcode::default() };
    }

    pub fn prompt(&self) -> &'static str {
        match self.step {
            Step::Overview => "",
            Step::Current => "Enter your passcode",
            Step::New if self.set => "Enter a new passcode",
            Step::New => "Choose a passcode",
            Step::Confirm => "Enter it again",
        }
    }

    /// A pad key was released. `sys` sends a command to /dev/violet/sys.
    pub fn key(&mut self, key: PadKey, sys: &mut dyn FnMut(&str) -> String) {
        self.note.clear();
        self.good = false;
        if !keypad::press(key, &mut self.typed) {
            return;
        }
        let typed = std::mem::take(&mut self.typed);
        match self.step {
            Step::Overview => {}
            Step::Current => {
                let answer = sys(&format!("passcode check {typed}"));
                if answer != "ok" {
                    self.note = explain(&answer);
                } else if self.removing {
                    match sys(&format!("passcode clear {typed}")).as_str() {
                        "ok" => self.finish(false, "Passcode turned off"),
                        answer => self.note = explain(answer),
                    }
                } else {
                    self.current = typed;
                    self.step = Step::New;
                }
            }
            Step::New => {
                self.first = typed;
                self.step = Step::Confirm;
            }
            Step::Confirm if typed != self.first => {
                self.first.clear();
                self.step = Step::New;
                self.note = "Those didn’t match. Try again.".into();
            }
            Step::Confirm => {
                let command = if self.set { format!("passcode set {typed} {}", self.current) } else { format!("passcode set {typed}") };
                match sys(&command).as_str() {
                    "ok" => self.finish(true, if self.set { "Passcode changed" } else { "Passcode set" }),
                    answer => {
                        self.note = explain(answer);
                        self.first.clear();
                        self.step = if self.set { Step::Current } else { Step::New };
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The runtime's side of the commands, with `code` as the passcode in force.
    fn runtime(code: &mut Option<String>, line: &str) -> String {
        let w: Vec<&str> = line.split_whitespace().collect();
        let right = |old: Option<&&str>| code.is_none() || old.map(|o| o.to_string()) == *code;
        match w.as_slice() {
            ["passcode", "check", old] => if right(Some(old)) { "ok" } else { "error wrong" }.into(),
            ["passcode", "set", new, rest @ ..] if right(rest.first()) => {
                *code = Some(new.to_string());
                "ok".into()
            }
            ["passcode", "clear", old] if right(Some(old)) => {
                *code = None;
                "ok".into()
            }
            _ => "error wrong".into(),
        }
    }

    fn type_code(p: &mut Passcode, code: &mut Option<String>, digits: &str) {
        let mut sys = |line: &str| runtime(code, line);
        for d in digits.bytes() {
            p.key(PadKey::Digit(d - b'0'), &mut sys);
        }
        p.key(PadKey::Ok, &mut sys);
    }

    #[test]
    fn choose_change_and_turn_off() {
        let mut code = None;
        let mut p = Passcode::new(false);
        p.begin(false);
        assert_eq!(p.prompt(), "Choose a passcode");
        type_code(&mut p, &mut code, "2580");
        assert_eq!(p.prompt(), "Enter it again");
        // A different second entry starts over.
        type_code(&mut p, &mut code, "2581");
        assert_eq!((p.step, code.is_none()), (Step::New, true));
        assert!(p.note.contains("match"));
        type_code(&mut p, &mut code, "2580");
        type_code(&mut p, &mut code, "2580");
        assert_eq!(code.as_deref(), Some("2580"));
        assert!(p.set && p.good && p.step == Step::Overview);

        // Changing it asks for the one in use first.
        p.begin(false);
        assert_eq!(p.prompt(), "Enter your passcode");
        type_code(&mut p, &mut code, "0000");
        assert_eq!((p.step, p.note.as_str()), (Step::Current, "Wrong passcode"));
        type_code(&mut p, &mut code, "2580");
        assert_eq!(p.prompt(), "Enter a new passcode");
        type_code(&mut p, &mut code, "135790");
        type_code(&mut p, &mut code, "135790");
        assert_eq!(code.as_deref(), Some("135790"));
        assert_eq!(p.note, "Passcode changed");

        p.begin(true);
        type_code(&mut p, &mut code, "2580");
        assert!(p.set && code.is_some());
        type_code(&mut p, &mut code, "135790");
        assert!(!p.set && code.is_none() && p.step == Step::Overview);
    }

    #[test]
    fn short_codes_wait_for_more_digits() {
        let mut code = None;
        let mut p = Passcode::new(false);
        p.begin(false);
        type_code(&mut p, &mut code, "12");
        assert_eq!((p.step, p.typed.as_str()), (Step::New, "12"));
    }

    #[test]
    fn waits() {
        assert_eq!(explain("error wait 30"), "Try again in 30 seconds");
        assert_eq!(explain("error wait 300"), "Try again in 5 minutes");
        assert_eq!(explain("error"), "Couldn’t save the passcode");
    }
}
