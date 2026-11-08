//! Conversation scripts checked on the existing GTK test thread.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;

use super::{PATCH_SCRIPT, WebView};

async fn evaluate(page: &WebView, script: &str) -> String {
    let answer = Rc::new(RefCell::new(None));
    let received = Rc::clone(&answer);
    page.eval(script, move |value| *received.borrow_mut() = Some(value));
    for _ in 0..200 {
        if let Some(value) = answer.borrow_mut().take() {
            return value.expect("the conversation script runs");
        }
        gtk::glib::timeout_future(Duration::from_millis(50)).await;
    }
    panic!("the conversation script did not finish");
}

pub(crate) async fn metadata_preserves_disclosures() {
    let history = r#"<details class="trimmed"><summary>Show trimmed content</summary>
<div style="height:1600px">Quoted history</div></details>"#;
    for body in [
        format!("<div class=\"body text\">{history}</div>"),
        format!(
            "<div class=\"body html\"><template shadowrootmode=\"open\">{history}</template></div>"
        ),
    ] {
        check_body(&body).await;
    }
}

async fn check_body(body: &str) {
    let page = WebView::reading("mailrs-patch-check", |request| request.refuse());
    let loaded = Rc::new(Cell::new(false));
    let ready = Rc::clone(&loaded);
    page.on_loaded(move || ready.set(true));
    let window = gtk::Window::builder()
        .default_width(500)
        .default_height(300)
        .child(&page.widget())
        .build();
    window.present();
    let article = format!(
        r#"<article id="m-read" class="message expanded unread">
<details><summary>Details</summary><p>Sender and recipients</p></details>
{body}</article>"#
    );
    page.load_html(&format!(
        "<!doctype html><html><body>{article}</body></html>"
    ));
    for _ in 0..200 {
        if loaded.get() {
            break;
        }
        gtk::glib::timeout_future(Duration::from_millis(50)).await;
    }
    assert!(loaded.get(), "the disclosure fixture loads");
    assert_eq!(
        evaluate(
            &page,
            "var message = document.getElementById('m-read'); \
            message.querySelector('details').open = true; \
            (message.querySelector('.body').shadowRoot || message.querySelector('.body')).querySelector('details').open = true; \
            window.scrollTo(0, 500); window.beforeScroll = window.scrollY; \
            String(window.beforeScroll > 0)"
        )
        .await,
        "true",
        "the reader scrolls into expanded quoted history"
    );
    let read = article.replace(" expanded unread", " expanded");
    let patches = serde_json::to_string(&[("read", read)]).unwrap();
    let script = [PATCH_SCRIPT[0], &patches, PATCH_SCRIPT[1]].concat();
    assert_eq!(evaluate(&page, &script).await, "done");
    let state = evaluate(
        &page,
        "var message = document.getElementById('m-read'); \
        JSON.stringify([message.classList.contains('unread'), \
        message.querySelector('details').open, \
        (message.querySelector('.body').shadowRoot || message.querySelector('.body')).querySelector('details').open, \
        window.scrollY, window.beforeScroll])",
    )
    .await;
    let (unread, details, history, scroll, before): (bool, bool, bool, f64, f64) =
        serde_json::from_str(&state).unwrap();
    assert!(!unread, "the unread mark clears");
    assert!(details, "marking read keeps sender details open");
    assert!(history, "marking read keeps quoted history open");
    assert!(
        (scroll - before).abs() < 1.0,
        "the reading position stays: {before} -> {scroll}"
    );
    evaluate(&page, "var message = document.getElementById('m-read'); \
        message.querySelector('details').open = false; \
        (message.querySelector('.body').shadowRoot || message.querySelector('.body')).querySelector('details').open = false").await;
    let patches = serde_json::to_string(&[("read", article)]).unwrap();
    let script = [PATCH_SCRIPT[0], &patches, PATCH_SCRIPT[1]].concat();
    assert_eq!(evaluate(&page, &script).await, "done");
    let closed = evaluate(&page, "var message = document.getElementById('m-read'); \
        JSON.stringify([message.classList.contains('unread'), \
        message.querySelector('details').open, \
        (message.querySelector('.body').shadowRoot || message.querySelector('.body')).querySelector('details').open])").await;
    let state: (bool, bool, bool) = serde_json::from_str(&closed).unwrap();
    window.close();
    assert_eq!(
        state,
        (true, false, false),
        "refresh keeps disclosures closed"
    );
}
