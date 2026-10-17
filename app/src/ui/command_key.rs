//! Command standing in for Control on macOS. Keys are named in GTK's
//! accelerator syntax with `<Control>`, and GDK reports the Command key as
//! `META_MASK` there, so a Control key also answers to Command on macOS.
//! Control keeps working too.

use gtk::gdk;
use gtk::prelude::*;

const MACOS: bool = cfg!(target_os = "macos");

/// A trigger for `accelerator`, which on macOS also fires for Command in
/// place of Control.
pub fn trigger(accelerator: &str) -> Option<gtk::ShortcutTrigger> {
    let control = gtk::ShortcutTrigger::parse_string(accelerator)?;
    match command_twin(MACOS, accelerator).and_then(|c| gtk::ShortcutTrigger::parse_string(&c)) {
        Some(command) => Some(gtk::AlternativeTrigger::new(control, command).upcast()),
        None => Some(control),
    }
}

/// `modifiers` with Command read as Control on macOS.
pub fn as_control(modifiers: gdk::ModifierType) -> gdk::ModifierType {
    command_as_control(MACOS, modifiers)
}

/// `accelerator` with `<Meta>` in place of `<Control>`, when it has one
/// and the platform is macOS.
fn command_twin(macos: bool, accelerator: &str) -> Option<String> {
    (macos && accelerator.contains("<Control>"))
        .then(|| accelerator.replacen("<Control>", "<Meta>", 1))
}

/// `modifiers` with Command read as Control when the platform is macOS.
fn command_as_control(macos: bool, modifiers: gdk::ModifierType) -> gdk::ModifierType {
    if macos && modifiers.contains(gdk::ModifierType::META_MASK) {
        (modifiers - gdk::ModifierType::META_MASK) | gdk::ModifierType::CONTROL_MASK
    } else {
        modifiers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_control_chord_has_a_command_twin_on_macos_alone() {
        assert_eq!(command_twin(true, "<Control>w").as_deref(), Some("<Meta>w"));
        assert_eq!(
            command_twin(true, "<Control><Shift>a").as_deref(),
            Some("<Meta><Shift>a")
        );
        assert_eq!(command_twin(true, "Escape"), None);
        assert_eq!(command_twin(true, "<Shift>F10"), None);
        assert_eq!(command_twin(false, "<Control>w"), None);
    }

    #[test]
    fn command_reads_as_control_on_macos_alone() {
        use gdk::ModifierType as M;
        assert_eq!(
            command_as_control(true, M::META_MASK | M::SHIFT_MASK),
            M::CONTROL_MASK | M::SHIFT_MASK
        );
        assert_eq!(command_as_control(true, M::CONTROL_MASK), M::CONTROL_MASK);
        assert_eq!(command_as_control(true, M::SHIFT_MASK), M::SHIFT_MASK);
        assert_eq!(command_as_control(false, M::META_MASK), M::META_MASK);
    }
}
