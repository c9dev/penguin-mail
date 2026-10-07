//! Drives a debug build on macOS from a script, without touching the
//! person's own mouse or keyboard.
//!
//! `PENGUIN_MAIL_DRIVE=script.txt` names the script. Each step is built as
//! an AppKit event and posted to this process's own queue, where GDK and
//! the web views take it as they would a real one; nothing passes through
//! the system's input, so the cursor stays where the person left it and
//! the window may sit behind others. `shot` saves the window with
//! `screencapture -l`, which reads a covered window as well, into the
//! folder `PENGUIN_MAIL_DRIVE_SHOTS` names, or the script's own.
//!
//! The steps, one a line, `#` starting a comment. Points count from the
//! window's top left:
//!
//! ```text
//! wait 500              milliseconds
//! window Penguin Mail   the window whose title holds these words
//! resize 900 600        the window's size, in points
//! click 528 350         also rclick, for the right button
//! scroll 700 400 -300   pixels, negative to go down
//! key f cmd             a key by name, with cmd, ctrl, alt or shift
//! type Lisbon           text, one key each
//! shot open-thread      saves open-thread.png
//! log anything          says it on stderr
//! quit
//! ```
//!
//! Linux has `scripts/demo-shot.sh`, which drives the app over the
//! accessibility bus instead.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gtk::glib;
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType, NSWindow};
use objc2_core_graphics::{CGEvent, CGEventField, CGScrollEventUnit};
use objc2_foundation::{NSPoint, NSString};

/// One line of a script.
#[derive(Debug, Clone, PartialEq)]
enum Step {
    Wait(u64),
    Window(String),
    Resize { width: f64, height: f64 },
    Click { x: f64, y: f64, right: bool },
    Scroll { x: f64, y: f64, dy: i32 },
    Key { name: String, modifiers: Vec<String> },
    Type(String),
    Shot(String),
    Log(String),
    Quit,
}

fn parse(script: &str) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    for (number, line) in script.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
        let rest = rest.trim();
        let numbers = || -> Result<Vec<f64>, String> {
            rest.split_whitespace()
                .map(|n| n.parse::<f64>().map_err(|_| format!("line {}: {n} is no number", number + 1)))
                .collect()
        };
        let step = match word {
            "wait" => Step::Wait(numbers()?.first().copied().unwrap_or(0.0) as u64),
            "window" => Step::Window(rest.to_string()),
            "resize" => match numbers()?[..] {
                [width, height] => Step::Resize { width, height },
                _ => return Err(format!("line {}: resize takes a width and a height", number + 1)),
            },
            "click" | "rclick" => match numbers()?[..] {
                [x, y] => Step::Click {
                    x,
                    y,
                    right: word == "rclick",
                },
                _ => return Err(format!("line {}: {word} takes x and y", number + 1)),
            },
            "scroll" => match numbers()?[..] {
                [x, y, dy] => Step::Scroll { x, y, dy: dy as i32 },
                _ => return Err(format!("line {}: scroll takes x, y and dy", number + 1)),
            },
            "key" => {
                let mut words = rest.split_whitespace().map(str::to_string);
                let Some(name) = words.next() else {
                    return Err(format!("line {}: key takes a name", number + 1));
                };
                Step::Key {
                    name,
                    modifiers: words.collect(),
                }
            }
            "type" => Step::Type(rest.to_string()),
            "shot" => Step::Shot(rest.to_string()),
            "log" => Step::Log(rest.to_string()),
            "quit" => Step::Quit,
            _ => return Err(format!("line {}: no step called {word}", number + 1)),
        };
        steps.push(step);
    }
    Ok(steps)
}

/// The AppKit key code and the text a key types, on a US layout.
fn key_of(name: &str) -> Option<(u16, String)> {
    let named = match name {
        "Return" => Some((36, "\r")),
        "Tab" => Some((48, "\t")),
        "space" => Some((49, " ")),
        "BackSpace" => Some((51, "\u{7f}")),
        "Escape" => Some((53, "\u{1b}")),
        "Left" => Some((123, "\u{f702}")),
        "Right" => Some((124, "\u{f703}")),
        "Down" => Some((125, "\u{f701}")),
        "Up" => Some((126, "\u{f700}")),
        "Page_Up" => Some((116, "\u{f72c}")),
        "Page_Down" => Some((121, "\u{f72d}")),
        "Home" => Some((115, "\u{f729}")),
        "End" => Some((119, "\u{f72b}")),
        _ => None,
    };
    if let Some((code, text)) = named {
        return Some((code, text.to_string()));
    }
    let mut chars = name.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return None;
    };
    const CODES: &[(char, u16)] = &[
        ('a', 0), ('s', 1), ('d', 2), ('f', 3), ('h', 4), ('g', 5), ('z', 6), ('x', 7),
        ('c', 8), ('v', 9), ('b', 11), ('q', 12), ('w', 13), ('e', 14), ('r', 15),
        ('y', 16), ('t', 17), ('1', 18), ('2', 19), ('3', 20), ('4', 21), ('6', 22),
        ('5', 23), ('=', 24), ('9', 25), ('7', 26), ('-', 27), ('8', 28), ('0', 29),
        (']', 30), ('o', 31), ('u', 32), ('[', 33), ('i', 34), ('p', 35), ('l', 37),
        ('j', 38), ('\'', 39), ('k', 40), (';', 41), ('\\', 42), (',', 43), ('/', 44),
        ('n', 45), ('m', 46), ('.', 47), ('`', 50), (' ', 49),
    ];
    let lower = c.to_ascii_lowercase();
    let code = CODES.iter().find(|(key, _)| *key == lower).map(|(_, code)| *code)?;
    Some((code, c.to_string()))
}

/// Starts the script `PENGUIN_MAIL_DRIVE` names, if it names one, once the
/// first window is up.
pub fn from_env() {
    let Some(path) = std::env::var_os("PENGUIN_MAIL_DRIVE").map(PathBuf::from) else {
        return;
    };
    let steps = match std::fs::read_to_string(&path)
        .map_err(|err| err.to_string())
        .and_then(|script| parse(&script))
    {
        Ok(steps) => steps,
        Err(err) => {
            eprintln!("drive: {}: {err}", path.display());
            return;
        }
    };
    let shots = std::env::var_os("PENGUIN_MAIL_DRIVE_SHOTS")
        .map(PathBuf::from)
        .or_else(|| path.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    glib::spawn_future_local(async move {
        let mut driver = Driver {
            window: None,
            title: "Penguin Mail".to_string(),
            shots,
        };
        // GTK starts on the first window, a moment after the loop does.
        while driver.target().is_none() {
            glib::timeout_future(Duration::from_millis(200)).await;
        }
        glib::timeout_future(Duration::from_millis(800)).await;
        for step in steps {
            driver.run(step).await;
        }
    });
}

struct Driver {
    window: Option<Retained<NSWindow>>,
    title: String,
    shots: PathBuf,
}

impl Driver {
    /// The window whose title holds the words asked for, found again
    /// whenever the one before closed.
    fn target(&mut self) -> Option<Retained<NSWindow>> {
        if let Some(window) = &self.window
            && window.isVisible()
        {
            return Some(window.clone());
        }
        let mtm = MainThreadMarker::new()?;
        let found = NSApplication::sharedApplication(mtm)
            .windows()
            .iter()
            .find(|window| window.isVisible() && window.title().to_string().contains(&self.title));
        self.window = found.clone();
        found
    }

    async fn run(&mut self, step: Step) {
        let pause = Duration::from_millis(120);
        match step {
            Step::Wait(ms) => glib::timeout_future(Duration::from_millis(ms)).await,
            Step::Window(title) => {
                self.title = title;
                self.window = None;
                if self.target().is_none() {
                    eprintln!("drive: no window called {:?}", self.title);
                }
            }
            Step::Resize { width, height } => {
                if let Some(window) = self.target() {
                    window.setContentSize(objc2_foundation::NSSize::new(width, height));
                }
                glib::timeout_future(Duration::from_millis(400)).await;
            }
            Step::Click { x, y, right } => {
                let (down, up) = match right {
                    true => (NSEventType::RightMouseDown, NSEventType::RightMouseUp),
                    false => (NSEventType::LeftMouseDown, NSEventType::LeftMouseUp),
                };
                self.mouse(NSEventType::MouseMoved, x, y);
                glib::timeout_future(Duration::from_millis(40)).await;
                self.mouse(down, x, y);
                glib::timeout_future(Duration::from_millis(40)).await;
                self.mouse(up, x, y);
                glib::timeout_future(pause).await;
            }
            Step::Scroll { x, y, dy } => {
                self.mouse(NSEventType::MouseMoved, x, y);
                // Ten small steps, as a wheel sends them.
                for _ in 0..10 {
                    self.scroll(x, y, dy / 10);
                    glib::timeout_future(Duration::from_millis(16)).await;
                }
                glib::timeout_future(pause).await;
            }
            Step::Key { name, modifiers } => {
                match key_of(&name) {
                    Some((code, text)) => self.key(code, &text, &modifiers),
                    None => eprintln!("drive: no key called {name}"),
                }
                glib::timeout_future(pause).await;
            }
            Step::Type(text) => {
                for c in text.chars() {
                    let (code, typed) = key_of(&c.to_string()).unwrap_or((0, c.to_string()));
                    let shift = c.is_ascii_uppercase().then(|| "shift".to_string());
                    self.key(code, &typed, &shift.into_iter().collect::<Vec<_>>());
                    glib::timeout_future(Duration::from_millis(30)).await;
                }
                glib::timeout_future(pause).await;
            }
            Step::Shot(name) => {
                // Let whatever the last step set off draw first.
                glib::timeout_future(Duration::from_millis(300)).await;
                self.shot(&name);
            }
            Step::Log(words) => eprintln!("drive: {words}"),
            Step::Quit => {
                eprintln!("drive: done");
                std::process::exit(0);
            }
        }
    }

    /// Where a point from the window's top left is in the window's own
    /// coordinates, which run from its bottom left.
    fn at(window: &NSWindow, x: f64, y: f64) -> NSPoint {
        let height = window.contentView().map_or(0.0, |view| view.frame().size.height);
        NSPoint::new(x, height - y)
    }

    fn mouse(&mut self, kind: NSEventType, x: f64, y: f64) {
        let Some(window) = self.target() else { return };
        let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            kind,
            Driver::at(&window, x, y),
            NSEventModifierFlags::empty(),
            now(),
            window.windowNumber(),
            None,
            0,
            1,
            if kind == NSEventType::MouseMoved { 0.0 } else { 1.0 },
        );
        post(event);
    }

    fn scroll(&mut self, x: f64, y: f64, dy: i32) {
        let Some(window) = self.target() else { return };
        let Some(event) = CGEvent::new_scroll_wheel_event2(None, CGScrollEventUnit::Pixel, 1, dy, 0, 0)
        else {
            return;
        };
        // CGEvent places points from the top left of the main screen.
        let on_screen = window.convertPointToScreen(Driver::at(&window, x, y));
        let top = window.screen().map_or(0.0, |screen| {
            let main = MainThreadMarker::new()
                .and_then(|mtm| objc2_app_kit::NSScreen::screens(mtm).firstObject())
                .unwrap_or(screen);
            main.frame().size.height
        });
        CGEvent::set_location(
            Some(&event),
            NSPoint::new(on_screen.x, top - on_screen.y),
        );
        let number = i64::try_from(window.windowNumber()).unwrap_or_default();
        // The window under the pointer, as the window server would say.
        CGEvent::set_integer_value_field(Some(&event), CGEventField(91), number);
        CGEvent::set_integer_value_field(Some(&event), CGEventField(92), number);
        post(NSEvent::eventWithCGEvent(&event));
    }

    fn key(&mut self, code: u16, text: &str, modifiers: &[String]) {
        let Some(window) = self.target() else { return };
        let mut flags = NSEventModifierFlags::empty();
        for modifier in modifiers {
            flags |= match modifier.as_str() {
                "cmd" => NSEventModifierFlags::Command,
                "ctrl" => NSEventModifierFlags::Control,
                "alt" => NSEventModifierFlags::Option,
                "shift" => NSEventModifierFlags::Shift,
                other => {
                    eprintln!("drive: no modifier called {other}");
                    NSEventModifierFlags::empty()
                }
            };
        }
        let typed = NSString::from_str(text);
        let plain = NSString::from_str(&text.to_lowercase());
        for kind in [NSEventType::KeyDown, NSEventType::KeyUp] {
            let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
                kind,
                NSPoint::new(0.0, 0.0),
                flags,
                now(),
                window.windowNumber(),
                None,
                &typed,
                &plain,
                false,
                code,
            );
            post(event);
        }
    }

    fn shot(&mut self, name: &str) {
        let Some(window) = self.target() else { return };
        let path = self.shots.join(format!("{name}.png"));
        let saved = std::process::Command::new("screencapture")
            .args(["-x", "-o", "-l", &window.windowNumber().to_string()])
            .arg(&path)
            .status();
        match saved {
            Ok(status) if status.success() => eprintln!("drive: shot {}", path.display()),
            other => eprintln!("drive: could not save {}: {other:?}", path.display()),
        }
    }
}

fn now() -> f64 {
    objc2_foundation::NSProcessInfo::processInfo().systemUptime()
}

/// Puts an event at the end of this process's own queue.
fn post(event: Option<Retained<NSEvent>>) {
    let (Some(event), Some(mtm)) = (event, MainThreadMarker::new()) else {
        return;
    };
    NSApplication::sharedApplication(mtm).postEvent_atStart(&event, false);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_script_reads_as_its_steps() {
        let steps = parse(
            "# open the first thread\nwait 500\nclick 528 350\nrclick 10 20\n\
             scroll 700 400 -300\nkey f cmd shift\ntype Lisbon\nshot open\nquit\n",
        )
        .unwrap();
        assert_eq!(
            steps,
            vec![
                Step::Wait(500),
                Step::Click { x: 528.0, y: 350.0, right: false },
                Step::Click { x: 10.0, y: 20.0, right: true },
                Step::Scroll { x: 700.0, y: 400.0, dy: -300 },
                Step::Key { name: "f".into(), modifiers: vec!["cmd".into(), "shift".into()] },
                Step::Type("Lisbon".into()),
                Step::Shot("open".into()),
                Step::Quit,
            ]
        );
    }

    #[test]
    fn a_line_it_cannot_read_says_where() {
        assert_eq!(parse("click 1").unwrap_err(), "line 1: click takes x and y");
        assert_eq!(parse("\nhop").unwrap_err(), "line 2: no step called hop");
    }

    #[test]
    fn keys_are_found_by_name_or_letter() {
        assert_eq!(key_of("f"), Some((3, "f".to_string())));
        assert_eq!(key_of("L"), Some((37, "L".to_string())));
        assert_eq!(key_of("Escape").map(|(code, _)| code), Some(53));
        assert_eq!(key_of("Hyper"), None);
    }
}
