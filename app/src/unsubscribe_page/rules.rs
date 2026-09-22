//! What to press on a page, decided from the page's data alone.
//!
//! [`pick`] fills in the shapes the owner's own newsletters use: a lone
//! button, a button under an address field, a preferences page with a
//! box that means everything. Anything else it leaves alone, because a
//! wrong press on a preferences page subscribes the person to something.
//! [`valid`] is the same rules read backwards, over a plan somebody else
//! wrote, and every plan goes through it before a key is typed.

use super::{Field, FieldKind, Form, PageForm, Pick, Plan, Unsure, words};

/// What the rules make of a page, with `address` as the only text they
/// will ever put into it.
pub fn pick(page: &PageForm, address: &str) -> Pick {
    // A captcha or a login is a question only the person can answer, and
    // it stops the page here: no plan, and no model asked either.
    if page.captcha {
        return Pick::Unsure(Unsure::Captcha);
    }
    if page.password || fields(page).any(|field| field.kind == FieldKind::Password) {
        return Pick::Unsure(Unsure::Login);
    }
    // A page that says the address is off the list is finished, whatever
    // else it offers. Some of these links unsubscribe on load and then
    // show a form for coming back.
    if says_done(page) {
        return Pick::AlreadyOff;
    }
    let Some(form) = the_form(page) else {
        return Pick::Unsure(if page.forms.is_empty() {
            Unsure::NoForm
        } else {
            Unsure::Ambiguous
        });
    };
    if form.fields.iter().any(asks_a_question) {
        return Pick::Unsure(Unsure::Ambiguous);
    }
    let fill = form
        .fields
        .iter()
        .filter(|field| field.value.trim().is_empty() && wants_the_address(field))
        .map(|field| (field.id, address.to_string()))
        .collect();
    let choices: Vec<&Field> = form.fields.iter().filter(|field| chooses(field)).collect();
    let tick: Vec<usize> = choices
        .iter()
        .filter(|field| words::means_all(&field.label))
        .map(|field| field.id)
        .collect();
    // A list of topics with no box meaning all of them is the one case
    // the rules give up on by design. Choosing between a sender's topics
    // is the person's to do, not the app's.
    if !choices.is_empty() && tick.is_empty() {
        return Pick::Unsure(Unsure::Ambiguous);
    }
    let mut presses = form
        .buttons
        .iter()
        .filter(|button| words::presses(&button.label));
    match (presses.next(), presses.next()) {
        (Some(button), None) => Pick::Submit(Plan {
            form: form.id,
            fill,
            tick,
            press: button.id,
        }),
        _ => Pick::Unsure(Unsure::Ambiguous),
    }
}

/// Whether the page says the address is off the list. The title counts,
/// because many of these pages put the whole answer in it.
pub fn says_done(page: &PageForm) -> bool {
    words::already_off(&page.text) || words::already_off(&page.title)
}

/// Whether a plan names only what the page holds and types only the
/// address. Every plan goes through this, the rules' own and a model's
/// alike.
pub fn valid(page: &PageForm, plan: &Plan, address: &str) -> bool {
    let Some(form) = page.forms.iter().find(|form| form.id == plan.form) else {
        return false;
    };
    if !form.buttons.iter().any(|button| button.id == plan.press) {
        return false;
    }
    let typed = plan.fill.iter().all(|(id, text)| {
        text == address
            && form
                .fields
                .iter()
                .any(|field| field.id == *id && wants_typing(field))
    });
    let ticked = plan.tick.iter().all(|id| {
        form.fields
            .iter()
            .any(|field| field.id == *id && chooses(field))
    });
    typed && ticked
}

/// The one form the rules will act on: the page's only form, or the one
/// form among several whose button reads as leaving a list.
fn the_form(page: &PageForm) -> Option<&Form> {
    if let [only] = page.forms.as_slice() {
        return Some(only);
    }
    let mut leaving = page.forms.iter().filter(|form| {
        form.buttons
            .iter()
            .any(|button| words::leaves(&button.label))
    });
    match (leaving.next(), leaving.next()) {
        (Some(form), None) => Some(form),
        _ => None,
    }
}

fn fields(page: &PageForm) -> impl Iterator<Item = &Field> {
    page.forms.iter().flat_map(|form| form.fields.iter())
}

/// A field the page insists on that nobody can answer from the message:
/// a list of options, or a box with nothing saying what belongs in it.
fn asks_a_question(field: &Field) -> bool {
    if !field.required {
        return false;
    }
    match field.kind {
        FieldKind::Select { .. } => true,
        // A hidden field carries the page's own token, and an email
        // field says what it wants by being one.
        FieldKind::Hidden | FieldKind::Email => false,
        _ => field.label.trim().is_empty(),
    }
}

/// Whether the address the newsletter was sent to belongs in this field.
fn wants_the_address(field: &Field) -> bool {
    match field.kind {
        FieldKind::Email => true,
        FieldKind::Text => words::asks_for_email(&field.label),
        _ => false,
    }
}

/// Whether anything at all may be typed into this field.
fn wants_typing(field: &Field) -> bool {
    matches!(field.kind, FieldKind::Email | FieldKind::Text)
}

fn chooses(field: &Field) -> bool {
    matches!(field.kind, FieldKind::Checkbox | FieldKind::Radio { .. })
}
