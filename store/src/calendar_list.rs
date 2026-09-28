//! Changes to an account's calendar list made on this computer, queued
//! until the provider takes them, and what a read of the provider's list
//! does around them. The events' own queue is in [`crate::calendar`].
//!
//! Each edit shows in the copy the moment it is made (the caller writes
//! it there with the functions below) and waits in
//! `calendar_list_changes` for the next send. A read of the provider's
//! list, which the send usually runs just ahead of, must not undo an edit
//! still waiting: [`save_calendar_list`] keeps a new calendar, a new name
//! or colour, a hidden flag and a deletion that have not gone out yet.

use std::collections::{HashMap, HashSet};

use mailrs_domain::AccountId;
use mailrs_domain::calendar::Calendar;
use mailrs_domain::calendar::list::ListEdit;
use rusqlite::{Connection, OptionalExtension, params};

use crate::Result;
use crate::calendar::{calendars, save_calendars};

/// One edit waiting in the queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedEdit {
    pub seq: i64,
    pub account_id: AccountId,
    /// The calendar's id when the edit was made, or since, once the
    /// provider named a new calendar.
    pub calendar: String,
    pub edit: ListEdit,
}

/// Queues `edit` for the calendar `calendar`. Deleting a calendar the
/// provider has not made yet drops every edit waiting for it instead,
/// since nothing of it has left this computer. Answers whether anything
/// is left to send.
pub fn enqueue_edit(conn: &Connection, account_id: AccountId, calendar: &str, edit: &ListEdit) -> Result<bool> {
    if *edit == ListEdit::Delete {
        let unsent = queued_edits(conn, account_id)?.iter().any(|q| q.calendar == calendar && q.edit.adds());
        if unsent {
            conn.execute(
                "DELETE FROM calendar_list_changes WHERE account_id = ?1 AND calendar = ?2",
                params![account_id, calendar],
            )?;
            return Ok(false);
        }
    }
    conn.execute(
        "INSERT INTO calendar_list_changes (account_id, calendar, edit) VALUES (?1, ?2, ?3)",
        params![account_id, calendar, serde_json::to_string(edit).unwrap_or_default()],
    )?;
    Ok(true)
}

/// The first edit waiting for the account.
pub fn next_edit(conn: &Connection, account_id: AccountId) -> Result<Option<QueuedEdit>> {
    Ok(queued_edits(conn, account_id)?.into_iter().next())
}

/// Every edit waiting for the account, oldest first.
pub fn queued_edits(conn: &Connection, account_id: AccountId) -> Result<Vec<QueuedEdit>> {
    let mut stmt = conn.prepare(
        "SELECT seq, calendar, edit FROM calendar_list_changes WHERE account_id = ?1 ORDER BY seq",
    )?;
    let rows = stmt.query_map(params![account_id], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
    })?;
    let mut queued = Vec::new();
    for row in rows {
        let (seq, calendar, edit) = row?;
        // A row this version cannot read would sit at the head of the
        // queue for good; leaving it out lets the rest go.
        if let Ok(edit) = serde_json::from_str(&edit) {
            queued.push(QueuedEdit { seq, account_id, calendar, edit });
        }
    }
    Ok(queued)
}

/// Takes one edit out of the queue, sent or turned down.
pub fn finish_edit(conn: &Connection, seq: i64) -> Result<()> {
    conn.execute("DELETE FROM calendar_list_changes WHERE seq = ?1", params![seq])?;
    Ok(())
}

/// Puts `calendar` on the account's list as the person made it, before
/// the provider has it. A calendar already there keeps its row.
pub fn add_calendar(conn: &Connection, account_id: AccountId, calendar: &Calendar) -> Result<()> {
    conn.execute(
        "INSERT INTO calendars (account_id, id, name, color, access, zone, is_primary, shown, reminders) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, '[]') ON CONFLICT (account_id, id) DO NOTHING",
        params![
            account_id,
            calendar.id,
            calendar.name,
            calendar.color,
            calendar.access.as_str(),
            calendar.zone,
            calendar.primary,
            calendar.shown,
        ],
    )?;
    Ok(())
}

/// Takes a calendar off the copy, with its events.
pub fn remove_calendar(conn: &Connection, account_id: AccountId, calendar: &str) -> Result<()> {
    conn.execute("DELETE FROM calendars WHERE account_id = ?1 AND id = ?2", params![account_id, calendar])?;
    Ok(())
}

pub fn set_name(conn: &Connection, account_id: AccountId, calendar: &str, name: &str) -> Result<()> {
    conn.execute(
        "UPDATE calendars SET name = ?3 WHERE account_id = ?1 AND id = ?2",
        params![account_id, calendar, name],
    )?;
    Ok(())
}

/// The provider's colour for a calendar, `#rrggbb`, which replaces a
/// colour of the person's own: it now shows on every device.
pub fn set_provider_color(conn: &Connection, account_id: AccountId, calendar: &str, color: &str) -> Result<()> {
    conn.execute(
        "UPDATE calendars SET color = ?3, own_color = NULL WHERE account_id = ?1 AND id = ?2",
        params![account_id, calendar, color],
    )?;
    Ok(())
}

/// Records the provider's hidden flag as it now stands, after the queue
/// sent a change to it.
pub fn set_provider_hidden(conn: &Connection, account_id: AccountId, calendar: &str, hidden: bool) -> Result<()> {
    conn.execute(
        "UPDATE calendars SET provider_hidden = ?3 WHERE account_id = ?1 AND id = ?2",
        params![account_id, calendar, hidden],
    )?;
    Ok(())
}

/// Files everything the copy holds under the calendar id `from` under
/// `to`: the calendar, its events and their guests, the reminders already
/// shown, and the changes still queued. A calendar made here goes out
/// under an id of this computer's own, and the provider answers with its
/// own.
pub fn rename_calendar_id(conn: &Connection, account_id: AccountId, from: &str, to: &str) -> Result<()> {
    if from == to {
        return Ok(());
    }
    // Events point at their calendar, and guests at their event, with no
    // cascade on a change of id. Checking the keys at the end of the
    // transaction lets every table move before any is checked.
    conn.pragma_update(None, "defer_foreign_keys", true)?;
    // A read that already brought the calendar in under the provider's
    // id holds nothing of the person's own yet; the row made here wins.
    conn.execute("DELETE FROM calendars WHERE account_id = ?1 AND id = ?2", params![account_id, to])?;
    let at = params![account_id, from, to];
    for table in ["calendars SET id", "events SET calendar", "event_guests SET calendar"] {
        let column = table.rsplit(' ').next().unwrap_or("calendar");
        conn.execute(&format!("UPDATE {table} = ?3 WHERE account_id = ?1 AND {column} = ?2"), at)?;
    }
    conn.execute(
        "UPDATE event_reminders_shown SET calendar = ?3 WHERE account_id = ?1 AND calendar = ?2",
        at,
    )?;
    conn.execute("UPDATE calendar_changes SET calendar = ?3 WHERE account_id = ?1 AND calendar = ?2", at)?;
    // A queued event carries its calendar in its body too, and a move
    // names its destination only there.
    for column in ["body", "prior_body", "restores"] {
        conn.execute(
            &format!(
                "UPDATE calendar_changes SET {column} = json_set({column}, '$.calendar', ?3) \
                 WHERE account_id = ?1 AND {column} IS NOT NULL AND json_extract({column}, '$.calendar') = ?2"
            ),
            at,
        )?;
    }
    conn.execute("UPDATE calendar_list_changes SET calendar = ?3 WHERE account_id = ?1 AND calendar = ?2", at)?;
    Ok(())
}

/// Replaces the account's calendar list with `list`, as the provider
/// reads it, around the edits still waiting to go out. A calendar the
/// provider hid or showed again since the last read, on another device,
/// leaves or joins the sidebar's list here too; one whose flag the
/// provider has not changed keeps the person's own choice.
pub fn save_calendar_list(conn: &Connection, account_id: AccountId, list: &[Calendar]) -> Result<()> {
    let queued = queued_edits(conn, account_id)?;
    let waiting = |calendar: &str, wanted: fn(&ListEdit) -> bool| {
        queued.iter().any(|q| q.calendar == calendar && wanted(&q.edit))
    };
    let held: HashMap<String, Calendar> =
        calendars(conn, account_id)?.into_iter().map(|c| (c.id.clone(), c)).collect();
    let mut kept: Vec<Calendar> = Vec::with_capacity(list.len());
    for calendar in list {
        if waiting(&calendar.id, |e| *e == ListEdit::Delete) {
            continue;
        }
        let mut calendar = calendar.clone();
        if let Some(mine) = held.get(&calendar.id) {
            if waiting(&calendar.id, |e| matches!(e, ListEdit::Rename { .. })) {
                calendar.name = mine.name.clone();
            }
            if waiting(&calendar.id, |e| matches!(e, ListEdit::Recolor { .. })) {
                calendar.color = mine.color.clone();
            }
        }
        kept.push(calendar);
    }
    let read: HashSet<String> = kept.iter().map(|c| c.id.clone()).collect();
    for q in queued.iter().filter(|q| q.edit.adds() && !read.contains(&q.calendar)) {
        if let Some(mine) = held.get(&q.calendar) {
            kept.push(mine.clone());
        }
    }
    save_calendars(conn, account_id, &kept)?;
    for calendar in list {
        if waiting(&calendar.id, |e| matches!(e, ListEdit::Hide { .. } | ListEdit::Delete)) {
            continue;
        }
        let before: Option<Option<bool>> = conn
            .query_row(
                "SELECT provider_hidden FROM calendars WHERE account_id = ?1 AND id = ?2",
                params![account_id, calendar.id],
                |row| row.get(0),
            )
            .optional()?;
        // A calendar new to the copy takes Google's flag; one whose flag
        // Google changed since the last read follows it. One read before
        // the flag was kept (NULL) keeps whatever the person chose here.
        let follow = match (held.contains_key(&calendar.id), before.flatten()) {
            (false, _) => calendar.hidden,
            (true, Some(was)) => was != calendar.hidden,
            (true, None) => false,
        };
        if follow {
            conn.execute(
                "UPDATE calendars SET listed = ?3, shown = ?3 WHERE account_id = ?1 AND id = ?2",
                params![account_id, calendar.id, !calendar.hidden],
            )?;
        }
        set_provider_hidden(conn, account_id, &calendar.id, calendar.hidden)?;
    }
    Ok(())
}

/// Whether the person took the calendar off the sidebar's list.
pub fn listed(conn: &Connection, account_id: AccountId, calendar: &str) -> Result<Option<bool>> {
    Ok(conn
        .query_row(
            "SELECT listed FROM calendars WHERE account_id = ?1 AND id = ?2",
            params![account_id, calendar],
            |row| row.get(0),
        )
        .optional()?)
}

#[cfg(test)]
mod tests {
    use mailrs_domain::calendar::{Access, Event};

    use super::*;
    use crate::accounts;
    use crate::calendar::{ChangeKind, enqueue, event, save_events, set_listed, set_own_color};

    fn calendar(id: &str, primary: bool) -> Calendar {
        Calendar {
            id: id.into(),
            name: id.into(),
            color: "#3584e4".into(),
            access: Access::Owner,
            zone: "UTC".into(),
            primary,
            shown: true,
            hidden: false,
            reminders: Vec::new(),
        }
    }

    fn store() -> (Connection, AccountId) {
        let conn = crate::open_in_memory().unwrap();
        let id = accounts::insert_account(&conn, "me@example.com", 0).unwrap();
        save_calendar_list(&conn, id, &[calendar("primary", true), calendar("team", false)]).unwrap();
        (conn, id)
    }

    fn ids(conn: &Connection, id: AccountId) -> Vec<String> {
        calendars(conn, id).unwrap().into_iter().map(|c| c.id).collect()
    }

    fn one(conn: &Connection, id: AccountId, calendar: &str) -> Calendar {
        calendars(conn, id).unwrap().into_iter().find(|c| c.id == calendar).unwrap()
    }

    #[test]
    fn edits_wait_in_the_order_they_were_made() {
        let (conn, id) = store();
        enqueue_edit(&conn, id, "team", &ListEdit::Rename { name: "Office".into() }).unwrap();
        enqueue_edit(&conn, id, "team", &ListEdit::Hide { hidden: true }).unwrap();
        let queued = queued_edits(&conn, id).unwrap();
        assert_eq!(queued.len(), 2);
        assert_eq!(queued[0].edit, ListEdit::Rename { name: "Office".into() });
        assert_eq!(next_edit(&conn, id).unwrap().unwrap().seq, queued[0].seq);
        finish_edit(&conn, queued[0].seq).unwrap();
        assert_eq!(next_edit(&conn, id).unwrap().unwrap().edit, ListEdit::Hide { hidden: true });
    }

    #[test]
    fn deleting_a_calendar_not_yet_made_drops_its_edits_unsent() {
        let (conn, id) = store();
        let made = ListEdit::Create { name: "Climbing".into(), color: "#16a766".into(), zone: String::new() };
        assert!(enqueue_edit(&conn, id, "new:a", &made).unwrap());
        enqueue_edit(&conn, id, "new:a", &ListEdit::Rename { name: "Bouldering".into() }).unwrap();
        assert!(!enqueue_edit(&conn, id, "new:a", &ListEdit::Delete).unwrap());
        assert!(queued_edits(&conn, id).unwrap().is_empty());
    }

    #[test]
    fn a_read_keeps_a_calendar_made_here_until_the_provider_has_it() {
        let (conn, id) = store();
        add_calendar(&conn, id, &Calendar { id: "new:a".into(), name: "Climbing".into(), ..calendar("new:a", false) })
            .unwrap();
        enqueue_edit(&conn, id, "new:a", &ListEdit::Create {
            name: "Climbing".into(),
            color: "#16a766".into(),
            zone: String::new(),
        })
        .unwrap();
        save_calendar_list(&conn, id, &[calendar("primary", true), calendar("team", false)]).unwrap();
        assert!(ids(&conn, id).contains(&"new:a".to_string()));
    }

    #[test]
    fn a_read_keeps_a_name_and_colour_still_waiting() {
        let (conn, id) = store();
        set_name(&conn, id, "team", "Office").unwrap();
        enqueue_edit(&conn, id, "team", &ListEdit::Rename { name: "Office".into() }).unwrap();
        set_provider_color(&conn, id, "team", "#16a766").unwrap();
        enqueue_edit(&conn, id, "team", &ListEdit::Recolor { color: "#16a766".into() }).unwrap();
        save_calendar_list(&conn, id, &[calendar("primary", true), calendar("team", false)]).unwrap();
        let team = one(&conn, id, "team");
        assert_eq!((team.name.as_str(), team.color.as_str()), ("Office", "#16a766"));
    }

    #[test]
    fn a_read_does_not_bring_back_a_calendar_whose_deletion_waits() {
        let (conn, id) = store();
        remove_calendar(&conn, id, "team").unwrap();
        enqueue_edit(&conn, id, "team", &ListEdit::Delete).unwrap();
        save_calendar_list(&conn, id, &[calendar("primary", true), calendar("team", false)]).unwrap();
        assert_eq!(ids(&conn, id), vec!["primary".to_string()]);
    }

    #[test]
    fn a_calendar_hidden_on_another_device_leaves_the_list() {
        let (conn, id) = store();
        let hidden = Calendar { hidden: true, ..calendar("team", false) };
        save_calendar_list(&conn, id, &[calendar("primary", true), hidden]).unwrap();
        assert_eq!(listed(&conn, id, "team").unwrap(), Some(false));
        assert!(!one(&conn, id, "team").shown);
    }

    #[test]
    fn a_calendar_shown_again_on_another_device_comes_back() {
        let (conn, id) = store();
        let hidden = Calendar { hidden: true, ..calendar("team", false) };
        save_calendar_list(&conn, id, &[calendar("primary", true), hidden]).unwrap();
        save_calendar_list(&conn, id, &[calendar("primary", true), calendar("team", false)]).unwrap();
        assert_eq!(listed(&conn, id, "team").unwrap(), Some(true));
    }

    /// A person who hid a calendar here while the account could not
    /// change Google's list keeps it hidden: Google's flag never moved.
    #[test]
    fn a_calendar_hidden_only_here_stays_hidden_while_the_provider_flag_is_unchanged() {
        let (conn, id) = store();
        set_listed(&conn, id, "team", false).unwrap();
        save_calendar_list(&conn, id, &[calendar("primary", true), calendar("team", false)]).unwrap();
        assert_eq!(listed(&conn, id, "team").unwrap(), Some(false));
    }

    #[test]
    fn a_hide_still_waiting_outlasts_a_read_of_the_old_flag() {
        let (conn, id) = store();
        set_listed(&conn, id, "team", false).unwrap();
        enqueue_edit(&conn, id, "team", &ListEdit::Hide { hidden: true }).unwrap();
        // The first read after the migration knows no earlier flag, and
        // Google has not taken the hide yet.
        save_calendar_list(&conn, id, &[calendar("primary", true), calendar("team", false)]).unwrap();
        assert_eq!(listed(&conn, id, "team").unwrap(), Some(false));
        set_provider_hidden(&conn, id, "team", true).unwrap();
        let hidden = Calendar { hidden: true, ..calendar("team", false) };
        save_calendar_list(&conn, id, &[calendar("primary", true), hidden]).unwrap();
        assert_eq!(listed(&conn, id, "team").unwrap(), Some(false));
    }

    #[test]
    fn a_new_id_takes_the_calendar_its_events_and_its_queued_changes_along() {
        let (conn, id) = store();
        add_calendar(&conn, id, &calendar("new:a", false)).unwrap();
        set_own_color(&conn, id, "new:a", Some("#16a766")).unwrap();
        let lesson = Event {
            calendar: "new:a".into(),
            id: "lesson".into(),
            title: "Lesson".into(),
            start: 1_000,
            end: 2_000,
            ..Event::default()
        };
        save_events(&conn, id, std::slice::from_ref(&lesson), 0).unwrap();
        enqueue(&conn, id, ChangeKind::Create, &lesson).unwrap();
        enqueue_edit(&conn, id, "new:a", &ListEdit::Rename { name: "Bouldering".into() }).unwrap();

        // `Db::write` runs every write in a transaction, which the
        // deferred key check needs.
        let tx = conn.unchecked_transaction().unwrap();
        rename_calendar_id(&tx, id, "new:a", "abc@group.calendar.google.com").unwrap();
        tx.commit().unwrap();

        assert!(!ids(&conn, id).contains(&"new:a".to_string()));
        assert_eq!(one(&conn, id, "abc@group.calendar.google.com").color, "#16a766");
        assert!(event(&conn, id, "abc@group.calendar.google.com", "lesson").unwrap().is_some());
        let queued = crate::calendar::queued(&conn, id).unwrap();
        assert_eq!(queued[0].calendar, "abc@group.calendar.google.com");
        assert_eq!(queued[0].body.as_ref().unwrap().calendar, "abc@group.calendar.google.com");
        assert_eq!(queued_edits(&conn, id).unwrap()[0].calendar, "abc@group.calendar.google.com");
    }

    #[test]
    fn removing_a_calendar_takes_its_events() {
        let (conn, id) = store();
        let retro = Event { calendar: "team".into(), id: "retro".into(), start: 1, end: 2, ..Event::default() };
        save_events(&conn, id, &[retro], 0).unwrap();
        remove_calendar(&conn, id, "team").unwrap();
        assert!(event(&conn, id, "team", "retro").unwrap().is_none());
    }

    #[test]
    fn the_provider_colour_replaces_an_own_one() {
        let (conn, id) = store();
        set_own_color(&conn, id, "team", Some("#fad165")).unwrap();
        set_provider_color(&conn, id, "team", "#16a766").unwrap();
        assert_eq!(one(&conn, id, "team").color, "#16a766");
    }
}
