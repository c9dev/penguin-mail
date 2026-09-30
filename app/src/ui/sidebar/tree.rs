//! An account's labels or folders as the sidebar lists them, and where
//! one lands when the person drags it or moves it from its menu. Free of
//! GTK, so plain tests check every rule; the sidebar and the window only
//! carry the answers out.

use std::collections::HashMap;

use mailrs_domain::{Label, LabelKind};

/// One of an account's own labels or folders as the sidebar lists it.
#[derive(Debug, PartialEq, Eq)]
pub struct LabelRow<'a> {
    pub label: &'a Label,
    /// The part of the name after the last slash.
    pub leaf: &'a str,
    /// 1 at the top, one more for each slash in the name.
    pub depth: u32,
    /// False for a group, which holds only other folders.
    pub opens: bool,
}

/// The name a label nests under: "Work" for "Work/Clients", none at the top.
fn parent(name: &str) -> Option<&str> {
    name.rsplit_once('/').map(|(parent, _)| parent)
}

fn leaf(name: &str) -> &str {
    name.rsplit_once('/').map_or(name, |(_, leaf)| leaf)
}

/// An account's labels and folders with the groups that hold folders, each
/// under its parent, so "Work/Clients" sits under "Work" even where the
/// server keeps no mail in "Work". Siblings go in the order the person
/// gave them, from `positions` by id, and the ones never moved follow by
/// name, ignoring case.
pub fn label_rows<'a>(labels: &'a [Label], positions: &HashMap<String, i64>) -> Vec<LabelRow<'a>> {
    let mut rows: Vec<LabelRow<'a>> = labels
        .iter()
        .filter(|l| matches!(l.kind, LabelKind::User | LabelKind::Group))
        .map(|label| LabelRow {
            label,
            leaf: leaf(&label.name),
            depth: 1 + label.name.matches('/').count() as u32,
            opens: label.kind == LabelKind::User,
        })
        .collect();
    let by_name: HashMap<&str, &str> = rows
        .iter()
        .map(|row| (row.label.name.as_str(), row.label.id.as_str()))
        .collect();
    // Each row sorts by the place of every label on its path, so a child
    // follows its parent and siblings keep the person's order among them.
    let place = |name: &str| -> Vec<(bool, i64, String, String)> {
        let ends = name.match_indices('/').map(|(at, _)| at).chain([name.len()]);
        ends.map(|end| {
            let path = &name[..end];
            let position = by_name.get(path).and_then(|id| positions.get(*id)).copied();
            let segment = leaf(path);
            (
                position.is_none(),
                position.unwrap_or(0),
                segment.to_lowercase(),
                segment.to_string(),
            )
        })
        .collect()
    };
    rows.sort_by_cached_key(|row| place(&row.label.name));
    rows
}

/// Which part of a row a dragged label is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    /// The upper third: before the row, beside it.
    Before,
    /// The middle third: inside the row's label.
    Inside,
    /// The lower third: after the row, beside it.
    After,
}

/// The zone at `y` down a row `height` tall.
pub fn zone(y: f64, height: f64) -> Zone {
    if y < height / 3.0 {
        Zone::Before
    } else if y > height * 2.0 / 3.0 {
        Zone::After
    } else {
        Zone::Inside
    }
}

/// Where a label goes: its whole new name, and its new siblings' ids in
/// order, itself among them under its present id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub name: String,
    pub order: Vec<String>,
}

/// Why a drop is turned down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The target is the label itself or nested under it.
    OwnSubtree,
    /// Another label already has this name at the target depth.
    Taken(String),
}

/// Where label `dragged` lands when dropped on `zone` of the row for
/// `target`. `rows` are the account's rows in sidebar order. `Ok(None)`
/// when the drop changes nothing.
pub fn place(
    rows: &[LabelRow<'_>],
    dragged: &str,
    target: &str,
    zone: Zone,
) -> Result<Option<Placement>, Refusal> {
    let find = |id: &str| rows.iter().find(|row| row.label.id == id);
    let (Some(moving), Some(onto)) = (find(dragged), find(target)) else {
        return Ok(None);
    };
    if dragged == target {
        return Ok(None);
    }
    let old = moving.label.name.as_str();
    if onto.label.name.starts_with(&format!("{old}/")) {
        return Err(Refusal::OwnSubtree);
    }
    let under = match zone {
        Zone::Inside => Some(onto.label.name.as_str()),
        Zone::Before | Zone::After => parent(&onto.label.name),
    };
    let name = match under {
        Some(under) => format!("{under}/{}", leaf(old)),
        None => leaf(old).to_string(),
    };
    // Gmail compares label names ignoring case, so two that differ only
    // in case would clash there.
    if name != old
        && rows
            .iter()
            .any(|row| row.label.id != dragged && row.label.name.eq_ignore_ascii_case(&name))
    {
        return Err(Refusal::Taken(name));
    }
    let now = siblings(rows, under);
    let mut order: Vec<String> = now.iter().filter(|id| *id != dragged).cloned().collect();
    let at = match zone {
        Zone::Inside => order.len(),
        Zone::Before | Zone::After => {
            let at = order.iter().position(|id| id == target).unwrap_or(order.len());
            if zone == Zone::After { at + 1 } else { at }
        }
    };
    order.insert(at.min(order.len()), dragged.to_string());
    if name == old && order == now {
        return Ok(None);
    }
    Ok(Some(Placement { name, order }))
}

/// The ids of the rows directly under `under`, or at the top for none, in
/// sidebar order.
fn siblings(rows: &[LabelRow<'_>], under: Option<&str>) -> Vec<String> {
    rows.iter()
        .filter(|row| parent(&row.label.name) == under)
        .map(|row| row.label.id.clone())
        .collect()
}

/// Moves label `id` one place up (`-1`) or down (`1`) among its
/// siblings, keeping its name. None at either end.
pub fn step(rows: &[LabelRow<'_>], id: &str, by: i32) -> Option<Placement> {
    let label = rows.iter().find(|row| row.label.id == id)?.label;
    let mut order = siblings(rows, parent(&label.name));
    let at = order.iter().position(|sibling| sibling == id)?;
    let to = at.checked_add_signed(by as isize).filter(|to| *to < order.len())?;
    order.swap(at, to);
    Some(Placement {
        name: label.name.clone(),
        order,
    })
}

/// Which of Move Up and Move Down a label's menu offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Moves {
    pub up: bool,
    pub down: bool,
}

/// The moves [`step`] can make for label `id`: none up for the first of
/// its siblings, none down for the last, and neither for an only child.
pub fn moves(rows: &[LabelRow<'_>], id: &str) -> Moves {
    Moves {
        up: step(rows, id, -1).is_some(),
        down: step(rows, id, 1).is_some(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use mailrs_domain::{Label, LabelKind};

    use super::{Placement, Refusal, Zone, label_rows, moves, place, step, zone};

    fn label(name: &str, kind: LabelKind) -> Label {
        Label {
            account_id: 1,
            id: format!("id:{name}"),
            name: name.to_string(),
            kind,
            color: None,
        }
    }

    /// Personal holds bills and Important, Work holds bugs, as the owner's
    /// Gmail shows them.
    fn owner_labels() -> Vec<Label> {
        ["Personal", "Personal/bills", "Personal/Important", "Work", "Work/bugs"]
            .into_iter()
            .map(|name| label(name, LabelKind::User))
            .collect()
    }

    fn names(labels: &[Label], positions: &HashMap<String, i64>) -> Vec<String> {
        label_rows(labels, positions)
            .iter()
            .map(|row| row.label.name.clone())
            .collect()
    }

    fn ids(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| format!("id:{name}")).collect()
    }

    fn placed(name: &str, order: &[&str]) -> Result<Option<Placement>, Refusal> {
        Ok(Some(Placement {
            name: name.to_string(),
            order: ids(order),
        }))
    }

    #[test]
    fn a_group_nests_its_folders_but_opens_nothing() {
        let labels = [
            label("Work/Clients", LabelKind::User),
            label("INBOX", LabelKind::System),
            label("Work", LabelKind::Group),
            label("receipts", LabelKind::User),
        ];
        let rows: Vec<(&str, u32, bool)> = label_rows(&labels, &HashMap::new())
            .iter()
            .map(|row| (row.leaf, row.depth, row.opens))
            .collect();
        assert_eq!(
            rows,
            [("receipts", 1, true), ("Work", 1, false), ("Clients", 2, true)]
        );
    }

    #[test]
    fn siblings_go_in_the_stored_order_and_the_rest_follow_by_name() {
        let labels = owner_labels();
        let positions = HashMap::from([
            ("id:Work".to_string(), 0),
            ("id:Personal/Important".to_string(), 0),
        ]);
        assert_eq!(
            names(&labels, &positions),
            ["Work", "Work/bugs", "Personal", "Personal/Important", "Personal/bills"]
        );
    }

    #[test]
    fn the_top_third_is_before_the_middle_inside_and_the_bottom_after() {
        assert_eq!(zone(2.0, 30.0), Zone::Before);
        assert_eq!(zone(15.0, 30.0), Zone::Inside);
        assert_eq!(zone(28.0, 30.0), Zone::After);
    }

    #[test]
    fn a_nested_label_dropped_beside_a_top_one_leaves_its_parent() {
        let labels = owner_labels();
        let rows = label_rows(&labels, &HashMap::new());
        assert_eq!(
            place(&rows, "id:Work/bugs", "id:Personal", Zone::Before),
            placed("bugs", &["Work/bugs", "Personal", "Work"])
        );
    }

    #[test]
    fn a_label_dropped_after_a_nested_one_joins_that_parent_beside_it() {
        let labels = owner_labels();
        let rows = label_rows(&labels, &HashMap::new());
        assert_eq!(
            place(&rows, "id:Work/bugs", "id:Personal/bills", Zone::After),
            placed("Personal/bugs", &["Personal/bills", "Work/bugs", "Personal/Important"])
        );
    }

    #[test]
    fn a_label_dropped_inside_another_nests_under_it_last() {
        let labels = owner_labels();
        let rows = label_rows(&labels, &HashMap::new());
        assert_eq!(
            place(&rows, "id:Work/bugs", "id:Personal", Zone::Inside),
            placed("Personal/bugs", &["Personal/bills", "Personal/Important", "Work/bugs"])
        );
    }

    #[test]
    fn a_top_label_moved_among_its_siblings_keeps_its_name() {
        let labels = owner_labels();
        let rows = label_rows(&labels, &HashMap::new());
        assert_eq!(
            place(&rows, "id:Work", "id:Personal", Zone::Before),
            placed("Work", &["Work", "Personal"])
        );
    }

    #[test]
    fn a_drop_where_the_label_already_is_changes_nothing() {
        let labels = owner_labels();
        let rows = label_rows(&labels, &HashMap::new());
        assert_eq!(place(&rows, "id:Work", "id:Work", Zone::Inside), Ok(None));
        assert_eq!(place(&rows, "id:Work", "id:Personal", Zone::After), Ok(None));
        assert_eq!(place(&rows, "id:Work/bugs", "id:Work", Zone::Inside), Ok(None));
    }

    #[test]
    fn a_label_cannot_go_inside_its_own_subtree() {
        let labels = owner_labels();
        let rows = label_rows(&labels, &HashMap::new());
        assert_eq!(
            place(&rows, "id:Personal", "id:Personal/bills", Zone::Inside),
            Err(Refusal::OwnSubtree)
        );
        assert_eq!(
            place(&rows, "id:Personal", "id:Personal/bills", Zone::Before),
            Err(Refusal::OwnSubtree)
        );
    }

    #[test]
    fn a_label_cannot_take_a_name_that_exists_at_the_target_depth() {
        let mut labels = owner_labels();
        labels.push(label("personal/BUGS", LabelKind::User));
        let rows = label_rows(&labels, &HashMap::new());
        assert_eq!(
            place(&rows, "id:Work/bugs", "id:Personal", Zone::Inside),
            Err(Refusal::Taken("Personal/bugs".into()))
        );
    }

    #[test]
    fn move_up_and_down_swap_a_label_with_its_neighbour_and_stop_at_the_ends() {
        let labels = owner_labels();
        let rows = label_rows(&labels, &HashMap::new());
        assert_eq!(
            step(&rows, "id:Personal/Important", -1),
            Some(Placement {
                name: "Personal/Important".into(),
                order: ids(&["Personal/Important", "Personal/bills"]),
            })
        );
        assert_eq!(step(&rows, "id:Personal/bills", -1), None);
        assert_eq!(step(&rows, "id:Work", 1), None);
    }

    #[test]
    fn a_label_offers_only_the_moves_that_have_somewhere_to_go() {
        let labels = owner_labels();
        let rows = label_rows(&labels, &HashMap::new());
        let offered = |id: &str| {
            let moves = moves(&rows, id);
            (moves.up, moves.down)
        };
        assert_eq!(offered("id:Personal"), (false, true));
        assert_eq!(offered("id:Work"), (true, false));
        assert_eq!(offered("id:Personal/Important"), (true, false));
        // The only label inside Work has nowhere to move.
        assert_eq!(offered("id:Work/bugs"), (false, false));
    }
}
