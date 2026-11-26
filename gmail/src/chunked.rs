//! Long secrets in the Windows Credential Manager, which keeps at most
//! 2560 bytes per credential: 1280 UTF-16 units. A Microsoft or Google
//! refresh token can run longer, so a long value goes in pieces. The
//! first entry, under the plain user name, then holds `chunks:<n>` and
//! `<user>#1` to `<user>#<n>` hold the parts. A short value stays in the
//! plain entry, as every other platform keeps it.
//!
//! shortcut: a secret that is literally `chunks:<n>` reads back as parts,
//! and no token or password looks like that; switch the marker to an
//! attribute if one ever does.

/// The most UTF-16 units one credential holds.
const PART: usize = 1280;
const MARK: &str = "chunks:";

/// One secret slot per user name, as the Credential Manager gives them.
pub trait Slots {
    fn get(&self, user: &str) -> Result<Option<String>, String>;
    fn set(&self, user: &str, value: &str) -> Result<(), String>;
    /// Succeeds when there is nothing to remove.
    fn remove(&self, user: &str) -> Result<(), String>;
}

/// The Credential Manager through `keyring`, under one service name.
#[cfg(windows)]
pub struct Native<'a>(pub &'a str);

#[cfg(windows)]
impl Native<'_> {
    fn entry(&self, user: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(self.0, user).map_err(|e| e.to_string())
    }
}

#[cfg(windows)]
impl Slots for Native<'_> {
    fn get(&self, user: &str) -> Result<Option<String>, String> {
        match self.entry(user)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn set(&self, user: &str, value: &str) -> Result<(), String> {
        self.entry(user)?.set_password(value).map_err(|e| e.to_string())
    }

    fn remove(&self, user: &str) -> Result<(), String> {
        match self.entry(user)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

fn part_user(user: &str, n: usize) -> String {
    format!("{user}#{n}")
}

/// How many parts `head` says the value has, `None` for a plain value.
fn parts_in(head: &str) -> Option<usize> {
    head.strip_prefix(MARK)?.parse().ok()
}

/// Cuts `value` into pieces of at most [`PART`] UTF-16 units, never in
/// the middle of a character.
fn split(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut start, mut units) = (0, 0);
    for (at, c) in value.char_indices() {
        if units + c.len_utf16() > PART {
            parts.push(&value[start..at]);
            (start, units) = (at, 0);
        }
        units += c.len_utf16();
    }
    parts.push(&value[start..]);
    parts
}

pub fn load(slots: &impl Slots, user: &str) -> Result<Option<String>, String> {
    let Some(head) = slots.get(user)? else {
        return Ok(None);
    };
    let Some(count) = parts_in(&head) else {
        return Ok(Some(head));
    };
    let mut value = String::new();
    for n in 1..=count {
        let part = slots
            .get(&part_user(user, n))?
            .ok_or_else(|| format!("part {n} of {count} is missing"))?;
        value.push_str(&part);
    }
    Ok(Some(value))
}

pub fn save(slots: &impl Slots, user: &str, value: &str) -> Result<(), String> {
    let old = slots.get(user)?.as_deref().and_then(parts_in).unwrap_or(0);
    let parts = split(value);
    let kept = if parts.len() == 1 {
        slots.set(user, value)?;
        0
    } else {
        for (i, part) in parts.iter().enumerate() {
            slots.set(&part_user(user, i + 1), part)?;
        }
        // The head goes last, so a failed save leaves the old value whole.
        slots.set(user, &format!("{MARK}{}", parts.len()))?;
        parts.len()
    };
    for n in kept + 1..=old {
        slots.remove(&part_user(user, n))?;
    }
    Ok(())
}

pub fn delete(slots: &impl Slots, user: &str) -> Result<(), String> {
    let old = slots.get(user)?.as_deref().and_then(parts_in).unwrap_or(0);
    for n in 1..=old {
        slots.remove(&part_user(user, n))?;
    }
    slots.remove(user)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    #[derive(Default)]
    struct Mock(RefCell<HashMap<String, String>>);

    impl Slots for Mock {
        fn get(&self, user: &str) -> Result<Option<String>, String> {
            Ok(self.0.borrow().get(user).cloned())
        }
        fn set(&self, user: &str, value: &str) -> Result<(), String> {
            assert!(value.encode_utf16().count() <= PART, "{user} is too long");
            self.0.borrow_mut().insert(user.into(), value.into());
            Ok(())
        }
        fn remove(&self, user: &str) -> Result<(), String> {
            self.0.borrow_mut().remove(user);
            Ok(())
        }
    }

    #[test]
    fn a_short_value_stays_in_one_entry() {
        let slots = Mock::default();
        save(&slots, "a", "short").unwrap();
        assert_eq!(slots.0.borrow().len(), 1);
        assert_eq!(load(&slots, "a").unwrap().as_deref(), Some("short"));
    }

    #[test]
    fn a_long_value_survives_a_round_trip() {
        let slots = Mock::default();
        let token = "é€".repeat(1900);
        save(&slots, "a", &token).unwrap();
        assert!(slots.0.borrow().len() > 2);
        assert_eq!(load(&slots, "a").unwrap(), Some(token));
    }

    #[test]
    fn a_shorter_value_removes_stale_parts() {
        let slots = Mock::default();
        save(&slots, "a", &"x".repeat(4000)).unwrap();
        save(&slots, "a", &"y".repeat(1500)).unwrap();
        assert_eq!(slots.0.borrow().len(), 3);
        save(&slots, "a", "tiny").unwrap();
        assert_eq!(slots.0.borrow().len(), 1);
        assert_eq!(load(&slots, "a").unwrap().as_deref(), Some("tiny"));
    }

    #[test]
    fn delete_removes_every_part() {
        let slots = Mock::default();
        save(&slots, "a", &"x".repeat(4000)).unwrap();
        delete(&slots, "a").unwrap();
        assert!(slots.0.borrow().is_empty());
        assert_eq!(load(&slots, "a").unwrap(), None);
    }
}
