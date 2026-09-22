//! The unsubscribe pages the demo's newsletters send people to, served
//! by the demo itself over the loopback.
//!
//! A way out has to be an http or https address: `sync::unsubscribe`
//! takes nothing else, and the hidden view would have to be let at the
//! file system to read a `file://` page, which is the one thing a view
//! that loads a stranger's page must not have. So the demo puts up a
//! small server of its own on a port the system picks, and the samples'
//! `List-Unsubscribe` headers point at it. Nothing here leaves the
//! computer, and the server is started only by `--demo`.
//!
//! There is one page of each shape the rules know: a lone button, a
//! button under an address field, a preferences page with a box meaning
//! all of it, and a list of topics with no such box, which is the shape
//! the rules leave to the person. The last one is the page the demo
//! opens in the browser.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::OnceLock;
use std::time::Duration;

/// How long a connection has to say what it wants before the thread
/// holding it gives up. A view that opens a socket and sends nothing is
/// a thread waiting for ever otherwise.
const PATIENCE: Duration = Duration::from_secs(5);

/// The most of a form submission the server reads. The pages answer the
/// same whatever came in; this only keeps the socket tidy.
const MOST_POSTED: usize = 64 * 1024;

/// Where the demo's pages are, as `http://127.0.0.1:port`. The first
/// call puts the server up; every later one answers from memory.
pub fn base() -> &'static str {
    static BASE: OnceLock<String> = OnceLock::new();
    BASE.get_or_init(start)
}

fn start() -> String {
    let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, 0)) {
        Ok(listener) => listener,
        Err(err) => {
            tracing::warn!(error = %err, "the demo could not serve its unsubscribe pages");
            // A link nobody answers loads the way a sender's own server
            // being down loads, and the run already has words for that.
            return "http://127.0.0.1:9".to_string();
        }
    };
    let port = listener.local_addr().map(|at| at.port()).unwrap_or(0);
    let _ = std::thread::Builder::new()
        .name("demo-pages".to_string())
        .spawn(move || serve(&listener));
    format!("http://127.0.0.1:{port}")
}

fn serve(listener: &TcpListener) {
    for stream in listener.incoming().flatten() {
        // A page may hold its connection open while a script runs, so
        // each one gets a thread. The demo makes a handful in a session.
        std::thread::spawn(move || {
            let _ = answer(stream);
        });
    }
}

fn answer(mut stream: TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(PATIENCE))?;
    stream.set_write_timeout(Some(PATIENCE))?;
    let mut reading = BufReader::new(stream.try_clone()?);
    let mut request = String::new();
    reading.read_line(&mut request)?;
    let mut posted = 0usize;
    loop {
        let mut header = String::new();
        if reading.read_line(&mut header)? == 0 || header.trim().is_empty() {
            break;
        }
        let lower = header.to_ascii_lowercase();
        if let Some(value) = lower.strip_prefix("content-length:") {
            posted = value.trim().parse().unwrap_or(0);
        }
    }
    if posted > 0 {
        let mut sent = vec![0; posted.min(MOST_POSTED)];
        reading.read_exact(&mut sent)?;
    }
    let mut words = request.split_whitespace();
    let method = words.next().unwrap_or_default().to_ascii_uppercase();
    let path = words
        .next()
        .unwrap_or("/")
        .split(['?', '#'])
        .next()
        .unwrap_or("/");
    match page(&method, path) {
        Some(page) => send(&mut stream, "200 OK", &page),
        None => send(&mut stream, "404 Not Found", &missing()),
    }
}

fn send(stream: &mut TcpStream, status: &str, page: &str) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n",
        page.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(page.as_bytes())?;
    stream.flush()
}

/// The page at `path`, if the demo has one. A POST is a form coming
/// back, and every form here answers with the page that says it worked.
fn page(method: &str, path: &str) -> Option<String> {
    match (method, path) {
        ("GET" | "HEAD", "/linden-books") => Some(one_button()),
        ("POST", "/linden-books/gone") => Some(gone(
            "Linden Books",
            "You have been unsubscribed. Linden Books will not write to you again.",
        )),
        ("GET" | "HEAD", "/trail-notes") => Some(email_confirm()),
        ("POST", "/trail-notes/gone") => Some(gone(
            "Trail Notes",
            "You have been unsubscribed. Thanks for walking with us.",
        )),
        ("GET" | "HEAD", "/beacon-outdoors") => Some(preferences()),
        ("POST", "/beacon-outdoors/saved") => Some(gone(
            "Beacon Outdoors",
            "You will no longer receive emails from Beacon Outdoors.",
        )),
        ("GET" | "HEAD", "/harbour-arts") => Some(topics()),
        // Whoever pressed Save here did it in their own browser, and
        // what they chose is theirs. The page says so and claims
        // nothing.
        ("POST", "/harbour-arts/saved") => Some(wrap(
            "Harbour City Arts mailing lists",
            "<h1>Your lists are saved</h1>\n<p>You will hear from the lists you left \
             ticked.</p>",
        )),
        _ => None,
    }
}

/// One page, in the plain shape a sender's own unsubscribe page has.
fn wrap(title: &str, body: &str) -> String {
    format!(
        "<!doctype html>\n\
         <html lang=\"en\">\n\
         <head>\n\
         <meta charset=\"utf-8\" />\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" />\n\
         <title>{title}</title>\n\
         <style>\n\
         body {{ font-family: system-ui, sans-serif; margin: 3rem auto; max-width: 34rem;\n\
         padding: 0 1.5rem; line-height: 1.5; color: #241f31; background: #fdfdfc; }}\n\
         h1 {{ font-size: 1.4rem; }}\n\
         label {{ display: block; margin: 0.6rem 0; }}\n\
         input[type=email] {{ display: block; width: 100%; padding: 0.5rem;\n\
         margin: 0.3rem 0 1rem; font: inherit; }}\n\
         button {{ font: inherit; padding: 0.5rem 1.2rem; }}\n\
         ul {{ list-style: none; padding: 0; }}\n\
         </style>\n\
         </head>\n\
         <body>\n{body}\n</body>\n\
         </html>\n"
    )
}

/// The commonest shape: the sender knows who the link belongs to, so
/// there is nothing to fill in and one button to press.
fn one_button() -> String {
    wrap(
        "Unsubscribe from Linden Books",
        "<h1>Unsubscribe from Linden Books</h1>\n\
         <p>Press the button and we will stop sending you our offers.</p>\n\
         <form method=\"post\" action=\"/linden-books/gone\">\n\
         <input type=\"hidden\" name=\"reader\" value=\"7c41\" required />\n\
         <button type=\"submit\">Unsubscribe</button>\n\
         </form>",
    )
}

/// A page that asks which address to take off the list before it does
/// anything, which is where the address the newsletter was sent to goes.
fn email_confirm() -> String {
    wrap(
        "Leave Trail Notes",
        "<h1>Leave Trail Notes</h1>\n\
         <p>Tell us the address you read Trail Notes at and we will take it off \
         the list.</p>\n\
         <form method=\"post\" action=\"/trail-notes/gone\">\n\
         <label for=\"address\">Email address</label>\n\
         <input type=\"email\" id=\"address\" name=\"address\" required />\n\
         <input type=\"hidden\" name=\"issue\" value=\"214\" required />\n\
         <button type=\"submit\">Confirm</button>\n\
         </form>",
    )
}

/// A preferences centre with a box that means all of it. It answers
/// where it stands rather than going anywhere, as many of these do.
fn preferences() -> String {
    wrap(
        "Beacon Outdoors email preferences",
        "<h1>Email preferences</h1>\n\
         <p>Choose what you would like to hear about.</p>\n\
         <form id=\"preferences\" method=\"post\" action=\"/beacon-outdoors/saved\">\n\
         <label for=\"address\">Your email</label>\n\
         <input type=\"email\" id=\"address\" name=\"address\" value=\"\" />\n\
         <ul>\n\
         <li><label><input type=\"checkbox\" name=\"topic\" value=\"gear\" checked /> \
         New gear</label></li>\n\
         <li><label><input type=\"checkbox\" name=\"topic\" value=\"trips\" checked /> \
         Guided trips</label></li>\n\
         <li><label><input type=\"checkbox\" name=\"none\" value=\"all\" /> \
         Unsubscribe from all emails</label></li>\n\
         </ul>\n\
         <button type=\"submit\">Save preferences</button>\n\
         </form>\n\
         <p id=\"said\"></p>\n\
         <script>\n\
         document.getElementById('preferences').addEventListener('submit', (event) => {\n\
           event.preventDefault();\n\
           const address = document.getElementById('address').value;\n\
           document.getElementById('said').textContent =\n\
             `You will no longer receive emails from Beacon Outdoors at ${address}.`;\n\
           document.getElementById('preferences').hidden = true;\n\
         });\n\
         </script>",
    )
}

/// A list of topics with no box meaning all of them. The rules give this
/// one up by design and it ends in the person's own browser.
fn topics() -> String {
    wrap(
        "Harbour City Arts mailing lists",
        "<h1>Your Harbour City Arts lists</h1>\n\
         <p>Untick the lists you would rather not hear from.</p>\n\
         <form method=\"post\" action=\"/harbour-arts/saved\">\n\
         <ul>\n\
         <li><label><input type=\"checkbox\" name=\"list\" value=\"music\" checked /> \
         Music</label></li>\n\
         <li><label><input type=\"checkbox\" name=\"list\" value=\"theatre\" checked /> \
         Theatre</label></li>\n\
         <li><label><input type=\"checkbox\" name=\"list\" value=\"galleries\" checked /> \
         Galleries</label></li>\n\
         <li><label><input type=\"checkbox\" name=\"list\" value=\"workshops\" checked /> \
         Workshops for young people</label></li>\n\
         </ul>\n\
         <button type=\"submit\">Save</button>\n\
         </form>",
    )
}

/// What a form that went in says back.
fn gone(sender: &str, said: &str) -> String {
    wrap(
        &format!("Unsubscribed from {sender}"),
        &format!("<h1>Unsubscribed from {sender}</h1>\n<p>{said}</p>"),
    )
}

fn missing() -> String {
    wrap(
        "Not found",
        "<h1>Not found</h1>\n<p>There is no such page here.</p>",
    )
}
