//! The page logic, with no WebKit and no window. Each fixture is one
//! shape the owner's own newsletters use, written as the extraction
//! script hands a page back, so a change to the rules shows up here
//! before it shows up on a real page.

use super::fake::{FakeAdviser, FakeBrowser};
use super::{
    Browser, Field, FieldKind, Outcome, PageError, PageForm, Pick, Plan, Step, Unsure, finish,
    pick, prepare, valid, words,
};

/// The address the newsletter was sent to, and the only text any of
/// these plans may type.
const ME: &str = "david@example.com";

const ONE_BUTTON: &str = include_str!("fixtures/one_button.json");
const EMAIL_CONFIRM: &str = include_str!("fixtures/email_confirm.json");
const PREFERENCES: &str = include_str!("fixtures/preferences.json");
const TOPICS: &str = include_str!("fixtures/topics.json");
const TWO_BUTTONS: &str = include_str!("fixtures/two_buttons.json");
const CAPTCHA: &str = include_str!("fixtures/captcha.json");
const LOGIN: &str = include_str!("fixtures/login.json");
const ALREADY_OFF: &str = include_str!("fixtures/already_off.json");
const PORTUGUESE: &str = include_str!("fixtures/portuguese.json");
const TWO_FORMS: &str = include_str!("fixtures/two_forms.json");
const REASONS: &str = include_str!("fixtures/reasons.json");

fn page(fixture: &str) -> PageForm {
    serde_json::from_str(fixture).expect("the fixture is a PageForm")
}

/// The plan the rules make of a fixture, which every test but the
/// unsure ones expects to exist.
fn plan(fixture: &str) -> Plan {
    plan_of(&page(fixture))
}

fn plan_of(page: &PageForm) -> Plan {
    match pick(page, ME) {
        Pick::Submit(plan) => plan,
        other => panic!("the rules gave up on this page: {other:?}"),
    }
}

#[test]
fn a_lone_button_gets_pressed() {
    assert_eq!(
        plan(ONE_BUTTON),
        Plan {
            form: 0,
            fill: Vec::new(),
            tick: Vec::new(),
            press: 2,
        }
    );
}

#[test]
fn an_address_field_gets_the_address_the_newsletter_came_to() {
    let plan = plan(EMAIL_CONFIRM);
    assert_eq!(plan.fill, [(1, ME.to_string())]);
    assert_eq!(plan.press, 3);
}

#[test]
fn the_box_that_means_every_list_gets_ticked() {
    let plan = plan(PREFERENCES);
    assert_eq!(plan.tick, [4], "only the box that means all of them");
    assert_eq!(plan.fill, [(1, ME.to_string())]);
    assert_eq!(plan.press, 5);
}

#[test]
fn a_box_asking_for_all_the_mail_is_not_the_box_that_means_leaving() {
    let mut page = page(PREFERENCES);
    page.forms[0].fields[3].label = "Send me all emails".to_string();
    assert_eq!(pick(&page, ME), Pick::Unsure(Unsure::Ambiguous));
}

#[test]
fn a_form_asking_why_gets_the_plain_reason_and_its_one_button() {
    assert_eq!(
        plan(REASONS),
        Plan {
            form: 0,
            fill: Vec::new(),
            tick: vec![2],
            press: 6,
        }
    );
}

#[test]
fn a_report_box_is_never_ticked_even_when_it_comes_first() {
    let mut page = page(REASONS);
    page.forms[0].fields.swap(1, 4);
    assert_eq!(plan_of(&page).tick, [2]);
}

#[test]
fn a_form_whose_only_reasons_are_reports_ticks_nothing() {
    let mut page = page(REASONS);
    page.forms[0].fields.retain(|field| field.id != 2 && field.id != 3);
    assert_eq!(plan_of(&page).tick, Vec::<usize>::new());
    assert_eq!(plan_of(&page).press, 6);
}

#[test]
fn a_required_group_of_reasons_picks_the_plain_one() {
    let reason = |id: usize, label: &str| Field {
        id,
        kind: FieldKind::Radio {
            group: "why".to_string(),
        },
        label: label.to_string(),
        required: true,
        ..Field::default()
    };
    let mut page = page(REASONS);
    page.forms[0].fields = vec![
        reason(2, "Too many emails"),
        reason(3, "Já não tenho interesse"),
        reason(4, "Isto é spam"),
        reason(5, "Outro"),
    ];
    assert_eq!(plan_of(&page).tick, [3]);
}

#[test]
fn a_list_of_topics_with_no_box_for_all_of_them_is_left_alone() {
    assert_eq!(pick(&page(TOPICS), ME), Pick::Unsure(Unsure::Ambiguous));
}

#[test]
fn two_buttons_that_both_read_as_leaving_are_a_question() {
    assert_eq!(
        pick(&page(TWO_BUTTONS), ME),
        Pick::Unsure(Unsure::Ambiguous)
    );
}

#[test]
fn a_captcha_stops_the_page_here() {
    assert_eq!(pick(&page(CAPTCHA), ME), Pick::Unsure(Unsure::Captcha));
}

#[test]
fn a_password_field_reads_as_a_page_wanting_an_account() {
    assert_eq!(pick(&page(LOGIN), ME), Pick::Unsure(Unsure::Login));
}

#[test]
fn a_page_saying_the_address_is_off_the_list_needs_no_plan() {
    assert_eq!(pick(&page(ALREADY_OFF), ME), Pick::AlreadyOff);
}

#[test]
fn a_page_with_no_form_at_all_says_so() {
    let mut empty = page(ALREADY_OFF);
    empty.title = String::new();
    empty.text = "Hello".to_string();
    assert_eq!(pick(&empty, ME), Pick::Unsure(Unsure::NoForm));
}

#[test]
fn portuguese_labels_read_the_same_way() {
    let plan = plan(PORTUGUESE);
    assert_eq!(
        plan.fill,
        [(1, ME.to_string())],
        "a text field labelled as an address takes one"
    );
    assert_eq!(plan.tick, [2]);
    assert_eq!(plan.press, 3);
}

#[test]
fn the_form_with_the_leaving_button_wins_over_the_search_box() {
    let plan = plan(TWO_FORMS);
    assert_eq!(plan.form, 3);
    assert_eq!(plan.press, 5);
    assert!(plan.fill.is_empty(), "the search box takes no address");
}

#[test]
fn a_required_list_of_options_is_a_question() {
    let mut page = page(EMAIL_CONFIRM);
    page.forms[0].fields.push(Field {
        id: 4,
        kind: FieldKind::Select {
            options: vec![
                "Too many emails".to_string(),
                "I never signed up".to_string(),
            ],
        },
        label: "Why are you leaving?".to_string(),
        required: true,
        ..Field::default()
    });
    assert_eq!(pick(&page, ME), Pick::Unsure(Unsure::Ambiguous));
}

#[test]
fn a_required_field_with_nothing_saying_what_it_wants_is_a_question() {
    let mut page = page(EMAIL_CONFIRM);
    page.forms[0].fields.push(Field {
        id: 4,
        kind: FieldKind::Text,
        required: true,
        ..Field::default()
    });
    assert_eq!(pick(&page, ME), Pick::Unsure(Unsure::Ambiguous));
}

#[test]
fn a_field_the_page_filled_in_is_left_as_it_is() {
    let mut page = page(EMAIL_CONFIRM);
    page.forms[0].fields[0].value = "someone@example.com".to_string();
    assert!(plan_of(&page).fill.is_empty());
}

#[test]
fn a_plan_typing_into_a_password_field_is_refused() {
    let page = page(LOGIN);
    let plan = Plan {
        form: 0,
        fill: vec![(2, ME.to_string())],
        tick: Vec::new(),
        press: 3,
    };
    assert!(!valid(&page, &plan, ME));
}

#[test]
fn a_plan_naming_a_field_that_is_not_there_is_refused() {
    let page = page(EMAIL_CONFIRM);
    let plan = Plan {
        form: 0,
        fill: vec![(99, ME.to_string())],
        tick: Vec::new(),
        press: 3,
    };
    assert!(!valid(&page, &plan, ME));
}

#[test]
fn a_plan_typing_anything_but_the_address_is_refused() {
    let page = page(EMAIL_CONFIRM);
    let plan = Plan {
        form: 0,
        fill: vec![(1, "x@evil.example".to_string())],
        tick: Vec::new(),
        press: 3,
    };
    assert!(!valid(&page, &plan, ME));
}

#[test]
fn a_plan_ticking_a_text_field_is_refused() {
    let page = page(PORTUGUESE);
    let plan = Plan {
        form: 0,
        fill: Vec::new(),
        tick: vec![1],
        press: 3,
    };
    assert!(!valid(&page, &plan, ME));
}

#[test]
fn a_plan_pressing_a_button_of_another_form_is_refused() {
    let page = page(TWO_FORMS);
    let plan = Plan {
        form: 3,
        fill: Vec::new(),
        tick: Vec::new(),
        press: 2,
    };
    assert!(!valid(&page, &plan, ME));
}

#[test]
fn the_rules_own_plans_pass_their_own_check() {
    for fixture in [
        ONE_BUTTON,
        EMAIL_CONFIRM,
        PREFERENCES,
        PORTUGUESE,
        TWO_FORMS,
        REASONS,
    ] {
        let page = page(fixture);
        assert!(valid(&page, &plan_of(&page), ME), "{}", page.url);
    }
}

const URL: &str = "https://news.shop.example/unsubscribe?u=9f2";

fn browser(fixture: &str) -> FakeBrowser {
    let mut page = page(fixture);
    page.url = URL.to_string();
    FakeBrowser::holding(URL, page)
}

#[tokio::test]
async fn a_page_the_rules_read_is_submitted_once_the_person_says_yes() {
    let browser = browser(ONE_BUTTON).answering("You're unsubscribed.");
    let prepared = prepare(&browser, None, URL, ME).await;
    assert!(matches!(prepared.step, Step::Submit(_)));
    assert!(
        browser.submissions().is_empty(),
        "preparing presses nothing"
    );
    assert_eq!(finish(&browser, &prepared).await, Outcome::Done);
    assert_eq!(browser.submissions().len(), 1);
    assert_eq!(browser.typed.borrow().as_slice(), [ME.to_string()]);
}

#[tokio::test]
async fn what_the_button_says_comes_back_for_the_confirmation_to_name() {
    let prepared = prepare(&browser(ONE_BUTTON), None, URL, ME).await;
    assert_eq!(prepared.button, "Unsubscribe");
    // A page nobody can press has no button to name.
    let prepared = prepare(&browser(TOPICS), None, URL, ME).await;
    assert!(prepared.button.is_empty());
}

#[tokio::test]
async fn a_page_that_says_nothing_after_the_form_went_in_is_unclear() {
    let browser = browser(ONE_BUTTON);
    let prepared = prepare(&browser, None, URL, ME).await;
    assert_eq!(finish(&browser, &prepared).await, Outcome::Unclear);
}

#[tokio::test]
async fn a_page_the_rules_give_up_on_goes_to_the_browser() {
    let browser = browser(TOPICS);
    let prepared = prepare(&browser, None, URL, ME).await;
    assert!(matches!(prepared.step, Step::Browser(ref url) if url == URL));
    assert_eq!(
        finish(&browser, &prepared).await,
        Outcome::OpenInBrowser(URL.to_string())
    );
    assert!(browser.submissions().is_empty());
}

#[tokio::test]
async fn an_adviser_that_invents_a_button_is_ignored() {
    let browser = browser(TOPICS);
    let adviser = FakeAdviser::saying(Some(Plan {
        form: 0,
        fill: Vec::new(),
        tick: vec![1],
        press: 42,
    }));
    let prepared = prepare(&browser, Some(&adviser), URL, ME).await;
    assert!(matches!(prepared.step, Step::Browser(_)));
}

#[tokio::test]
async fn an_advisers_plan_that_names_what_the_page_holds_is_submitted() {
    let browser = browser(TOPICS).answering("You have been removed from every list.");
    let adviser = FakeAdviser::saying(Some(Plan {
        form: 0,
        fill: Vec::new(),
        tick: vec![1, 2, 3],
        press: 4,
    }));
    let prepared = prepare(&browser, Some(&adviser), URL, ME).await;
    assert!(matches!(prepared.step, Step::Submit(_)));
    assert_eq!(finish(&browser, &prepared).await, Outcome::Done);
}

#[tokio::test]
async fn a_captcha_never_reaches_the_adviser() {
    let browser = browser(CAPTCHA);
    let adviser = FakeAdviser::saying(None);
    let prepared = prepare(&browser, Some(&adviser), URL, ME).await;
    assert!(matches!(prepared.step, Step::Browser(_)));
    assert_eq!(adviser.asked.get(), 0, "a model sees no captcha page");
}

#[tokio::test]
async fn a_login_never_reaches_the_adviser() {
    let browser = browser(LOGIN);
    let adviser = FakeAdviser::saying(None);
    prepare(&browser, Some(&adviser), URL, ME).await;
    assert_eq!(adviser.asked.get(), 0);
}

#[tokio::test]
async fn a_page_that_unsubscribed_on_load_is_done_with_no_submission() {
    let browser = browser(ALREADY_OFF);
    let prepared = prepare(&browser, None, URL, ME).await;
    assert!(matches!(prepared.step, Step::AlreadyOff));
    assert_eq!(finish(&browser, &prepared).await, Outcome::Done);
    assert!(browser.submissions().is_empty());
}

#[tokio::test]
async fn a_page_that_will_not_load_goes_to_the_browser() {
    let mut browser = browser(ONE_BUTTON);
    browser.fail = Some(PageError::Timeout);
    let prepared = prepare(&browser, None, URL, ME).await;
    assert_eq!(
        finish(&browser, &prepared).await,
        Outcome::OpenInBrowser(URL.to_string())
    );
}

/// Where the list after this one sent the view while the dialog was up.
const NEXT: &str = "https://other.example/leave";

fn holding_two(first: &str, second: &str) -> FakeBrowser {
    let mut browser = browser(first);
    let mut next = page(second);
    next.url = NEXT.to_string();
    browser.pages.insert(NEXT.to_string(), next);
    browser
}

#[tokio::test]
async fn a_page_the_next_list_pushed_aside_is_put_back_before_it_is_pressed() {
    let browser = holding_two(ONE_BUTTON, PREFERENCES).answering("You're unsubscribed.");
    let prepared = prepare(&browser, None, URL, ME).await;
    // The batch reads every page before it asks, so the view stands on
    // the last list's page by the time the person says yes.
    prepare(&browser, None, NEXT, ME).await;
    assert_eq!(browser.at(), NEXT);
    assert_eq!(finish(&browser, &prepared).await, Outcome::Done);
    assert_eq!(browser.at(), URL, "the list's own page is pressed");
    assert_eq!(browser.submissions(), [plan(ONE_BUTTON)]);
}

#[tokio::test]
async fn a_page_that_changed_while_it_waited_is_not_pressed() {
    let mut browser = holding_two(ONE_BUTTON, PREFERENCES);
    let prepared = prepare(&browser, None, URL, ME).await;
    prepare(&browser, None, NEXT, ME).await;
    // The sender rebuilt the page between the reading and the yes, so
    // the ids in the plan now name other things.
    let mut changed = page(TOPICS);
    changed.url = URL.to_string();
    browser.pages.insert(URL.to_string(), changed);
    let Outcome::Failed(why) = finish(&browser, &prepared).await else {
        panic!("a page that changed is not one to press blind");
    };
    assert!(why.contains("changed"), "{why}");
    assert!(browser.submissions().is_empty());
}

#[tokio::test]
async fn a_submission_that_times_out_fails() {
    let mut browser = browser(ONE_BUTTON);
    let prepared = prepare(&browser, None, URL, ME).await;
    browser.fail = Some(PageError::Timeout);
    let Outcome::Failed(why) = finish(&browser, &prepared).await else {
        panic!("a timeout is a failure");
    };
    assert!(why.contains("20 seconds"), "{why}");
}

#[test]
fn a_word_matches_whole_and_not_in_the_middle_of_another() {
    assert!(words::leaves("Unsubscribe"));
    assert!(!words::already_off("Press here to unsubscribe"));
    assert!(words::already_off("You have been unsubscribed."));
}

#[test]
fn a_page_saying_it_removed_the_address_reads_as_done_however_it_words_it() {
    for said in [
        "You have been successfully removed from this subscriber list.",
        "You have been successfully unsubscribed.",
        "Your email address has now been removed.",
        "You are now unsubscribed from our newsletter.",
        "We have removed you from this mailing list.",
    ] {
        assert!(words::already_off(said), "{said}");
    }
}

#[test]
fn a_page_that_only_mentions_removal_does_not_read_as_done() {
    for said in [
        "Click below and you will be instantly removed.",
        "Choose the emails you want to be unsubscribed from.",
    ] {
        assert!(!words::already_off(said), "{said}");
    }
}

#[test]
fn accents_and_punctuation_make_no_difference() {
    assert!(words::leaves("Cancelar subscricao"));
    assert!(words::leaves("OPT-OUT"));
    assert!(words::already_off("A sua subscrição foi cancelada!"));
}
