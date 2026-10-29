//! The link destination belongs to the reader's interface, outside mail's styles.

use mailrs_domain::translate::gettext;
use serde::Deserialize;

use crate::link_safety::{self, Concern};
use crate::web::WebView;

#[derive(Deserialize)]
struct Hover {
    load: String,
    serial: u64,
    href: String,
    text: String,
}

pub fn attach(page: &WebView) {
    page.add_script(include_str!("link_preview.js"));
    let weak = page.downgrade();
    page.on_message("mailrsLink", move |message| {
        let (Some(page), Ok(hover)) = (weak.upgrade(), serde_json::from_str::<Hover>(message))
        else {
            return;
        };
        let link = link_safety::inspect(&hover.href, &hover.text);
        let answer = link.map(|link| {
            let reason = match link.concern {
                Some(Concern::DifferentSite) => {
                    gettext("The displayed address points to a different site.")
                }
                Some(Concern::Credentials) => {
                    gettext("This address contains a username that can disguise the destination.")
                }
                Some(Concern::HiddenCharacters) => {
                    gettext("This link contains hidden or direction-changing characters.")
                }
                None => String::new(),
            };
            serde_json::json!({ "site": link.site, "address": link.address, "reason": reason })
        });
        let reply =
            serde_json::json!({ "load": hover.load, "serial": hover.serial, "link": answer });
        page.run(&format!("window.mailrsShowLinkPreview?.({reply})"));
    });
}

pub fn clear(page: &WebView) {
    page.run("window.mailrsHideLinkPreview?.()");
}

#[cfg(test)]
pub mod checks {
    use super::*;
    use gtk::glib;
    use gtk::prelude::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::Duration;

    async fn eval(page: &WebView, script: &str) -> String {
        let (send, receive) = futures::channel::oneshot::channel();
        page.eval(script, move |answer| {
            let _ = send.send(answer);
        });
        receive.await.unwrap().unwrap()
    }

    pub fn run() {
        glib::MainContext::default().block_on(async {
            let loaded = Rc::new(Cell::new(false));
            let ready = loaded.clone();
            let page = WebView::reading("mailrs-link-check", |request| request.refuse());
            let window = gtk::Window::builder().default_width(600).default_height(400).build();
            window.set_child(Some(&page.widget()));
            window.present();
            attach(&page);
            page.on_loaded(move || ready.set(true));
            let theme = crate::render::Theme {
                dark: false, accent: "#e8660c".into(), accent_text: "#a64600".into(),
                summarize: false, font: String::new(), colors: None,
            };
            let mut html = crate::render::head(&crate::render::Head {
                subject: "Links", count: 1, allow_remote: false,
            }, &theme);
            html.push_str(r#"<div class="body-frame"><div id="body"><template shadowrootmode="open"><a id="ordinary" href="https://example.com/read">Read more</a><a id="link" href="https://evil.example/login"><b>https://bank.example</b></a><div style="height:1800px"></div><a id="bottom" href="https://bottom.example">Bottom</a></template></div></div>"#);
            html.push_str(crate::render::TAIL);
            page.load_html(&html);
            for _ in 0..100 {
                if loaded.get() { break; }
                glib::timeout_future(Duration::from_millis(20)).await;
            }
            assert!(loaded.get(), "the message loads");
            // WebKit can keep native hover events inside the message's
            // shadow root. Ordinary links still need a destination preview.
            eval(&page, "document.querySelector('#body').shadowRoot.querySelector('#ordinary').dispatchEvent(new MouseEvent('mouseover',{bubbles:true,composed:false}));'sent'").await;
            for _ in 0..100 {
                if eval(&page, "String(!document.getElementById('mailrs-link-preview').hidden)").await == "true" { break; }
                glib::timeout_future(Duration::from_millis(20)).await;
            }
            assert_eq!(eval(&page, "String(!document.getElementById('mailrs-link-preview').hidden && document.getElementById('mailrs-link-preview').textContent.includes('https://example.com/read'))").await, "true", "an ordinary link shows its destination without crossing the shadow boundary");
            assert_eq!(eval(&page, "String(document.getElementById('mailrs-link-preview').classList.contains('caution') || !!document.querySelector('#mailrs-link-preview small'))").await, "false", "ordinary links have no warning");
            eval(&page, "document.querySelector('#body').shadowRoot.querySelector('b').dispatchEvent(new MouseEvent('mouseover',{bubbles:true,composed:true}));'sent'").await;
            let mut shown = String::new();
            for _ in 0..100 {
                shown = eval(&page, "document.getElementById('mailrs-link-preview').hidden ? '' : document.getElementById('mailrs-link-preview').textContent").await;
                if !shown.is_empty() { break; }
                glib::timeout_future(Duration::from_millis(20)).await;
            }
            assert!(shown.contains("evil.example"), "{shown}");
            assert!(shown.contains("different site"), "{shown}");
            // The preview's own words are a change under body. They stay.
            eval(&page, "document.getElementById('mailrs-link-preview').appendChild(document.createElement('span')); 'kept'").await;
            assert_eq!(eval(&page, "String(document.getElementById('mailrs-link-preview').hidden)").await, "false");
            // A patch replaces an article that is not a direct child of body.
            eval(&page, "document.querySelector('.body-frame').appendChild(document.createElement('article')); 'patched'").await;
            for _ in 0..100 {
                if eval(&page, "String(document.getElementById('mailrs-link-preview').hidden)").await == "true" {
                    break;
                }
                glib::timeout_future(Duration::from_millis(20)).await;
            }
            assert_eq!(eval(&page, "String(document.getElementById('mailrs-link-preview').hidden)").await, "true");
            // A body fetched later introduces another shadow root.
            eval(&page, "const host=document.createElement('div');host.id='later';document.querySelector('.body-frame').appendChild(host);host.attachShadow({mode:'open'}).innerHTML='<a href=\"https://later.example/read\">Later link</a>'; 'patched'").await;
            glib::timeout_future(Duration::from_millis(20)).await;
            eval(&page, "document.querySelector('#later').shadowRoot.querySelector('a').dispatchEvent(new MouseEvent('mouseover',{bubbles:true,composed:false}));'sent'").await;
            for _ in 0..100 {
                if eval(&page, "String(!document.getElementById('mailrs-link-preview').hidden && document.getElementById('mailrs-link-preview').textContent.includes('later.example'))").await == "true" { break; }
                glib::timeout_future(Duration::from_millis(20)).await;
            }
            assert_eq!(eval(&page, "String(!document.getElementById('mailrs-link-preview').hidden && document.getElementById('mailrs-link-preview').textContent.includes('later.example'))").await, "true", "a newly inserted message gets hover previews");
            clear(&page);
            assert_eq!(eval(&page, "String(document.getElementById('mailrs-link-preview').hidden)").await, "true");
            eval(&page, "document.querySelector('#body').shadowRoot.querySelector('a').dispatchEvent(new FocusEvent('focusin',{bubbles:true,composed:true}));'sent'").await;
            for _ in 0..100 {
                if eval(&page, "String(document.getElementById('mailrs-link-preview').hidden)").await == "false" { break; }
                glib::timeout_future(Duration::from_millis(20)).await;
            }
            assert_eq!(eval(&page, "String(document.getElementById('mailrs-link-preview').hidden)").await, "false");
            // Focusing a link below the viewport scrolls it into view. Its
            // preview must survive both that scroll and later reader scrolling.
            eval(&page, "document.querySelector('#body').shadowRoot.querySelector('#bottom').focus(); 'focused'").await;
            for _ in 0..100 {
                glib::timeout_future(Duration::from_millis(20)).await;
                if eval(&page, "String(!document.getElementById('mailrs-link-preview').hidden && document.getElementById('mailrs-link-preview').textContent.includes('bottom.example') && scrollY > 0)").await == "true" { break; }
            }
            assert_eq!(eval(&page, "String(!document.getElementById('mailrs-link-preview').hidden && scrollY > 0)").await, "true", "a focused link keeps its destination after automatic scrolling");
            eval(&page, "window.scrollTo(0,0); 'scrolled'").await;
            glib::timeout_future(Duration::from_millis(100)).await;
            assert_eq!(eval(&page, "String(document.getElementById('mailrs-link-preview').hidden)").await, "false", "scrolling keeps a keyboard-focused destination");
            eval(&page, "document.querySelector('#body').shadowRoot.querySelector('#bottom').blur(); 'blurred'").await;
            glib::timeout_future(Duration::from_millis(50)).await;
            assert_eq!(eval(&page, "String(document.getElementById('mailrs-link-preview').hidden)").await, "true");
            // An old answer cannot replace a later hover or a cleared preview.
            eval(&page, "window.mailrsShowLinkPreview({load:'old',serial:3,link:{site:'stale.example',address:'bad',reason:''}});window.dispatchEvent(new Event('blur'));window.mailrsShowLinkPreview({load:'',serial:3,link:{site:'stale.example',address:'bad',reason:''}});'done'").await;
            assert_eq!(eval(&page, "String(document.getElementById('mailrs-link-preview').hidden)").await, "true");
            window.close();
        });
    }
}
