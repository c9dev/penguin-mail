//! The XML WebDAV, CalDAV and CardDAV speak: the request bodies Penguin
//! Mail sends, and the multistatus answers read into [`Props`]. roxmltree
//! refuses a document type declaration, so no answer can expand entities,
//! and the node count is capped.

use roxmltree::{Document, Node, ParsingOptions};

use crate::DavError;

const DAV: &str = "DAV:";
const CALDAV: &str = "urn:ietf:params:xml:ns:caldav";
const CARDDAV: &str = "urn:ietf:params:xml:ns:carddav";
const CS: &str = "http://calendarserver.org/ns/";
const APPLE: &str = "http://apple.com/ns/ical/";

/// Far above any answer under [`crate::MOST_BYTES`].
const MOST_NODES: u32 = 1_000_000;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Multistatus {
    pub responses: Vec<DavResponse>,
    /// A `sync-collection` answer's new token.
    pub sync_token: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DavResponse {
    pub href: String,
    /// The response's own status, which `sync-collection` sets to 404 for
    /// a member that went.
    pub status: Option<u16>,
    /// The properties the server answered with a 2xx.
    pub props: Props,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Props {
    pub displayname: Option<String>,
    /// Apple's `calendar-color`, as the server wrote it (`#RRGGBBAA`).
    pub color: Option<String>,
    pub is_calendar: bool,
    pub is_addressbook: bool,
    /// `supported-calendar-component-set`, such as `VEVENT`.
    pub components: Vec<String>,
    /// `current-user-privilege-set` holds `write`, `write-content` or `all`.
    pub can_write: bool,
    pub getctag: Option<String>,
    pub sync_token: Option<String>,
    /// As the server wrote it, quotes and all, since If-Match sends it back.
    pub etag: Option<String>,
    pub calendar_data: Option<String>,
    pub address_data: Option<String>,
    pub principal: Option<String>,
    pub calendar_home: Option<String>,
    pub addressbook_home: Option<String>,
    /// `calendar-timezone`: a VCALENDAR holding one VTIMEZONE.
    pub timezone: Option<String>,
    /// `supported-report-set` lists `sync-collection`.
    pub reports_sync: bool,
}

fn options<'a>() -> ParsingOptions<'a> {
    ParsingOptions { allow_dtd: false, nodes_limit: MOST_NODES, ..ParsingOptions::default() }
}

fn is(node: Node, namespace: &str, name: &str) -> bool {
    node.is_element() && node.tag_name().name() == name && node.tag_name().namespace() == Some(namespace)
}

fn child<'a, 'i>(node: Node<'a, 'i>, namespace: &str, name: &str) -> Option<Node<'a, 'i>> {
    node.children().find(|c| is(*c, namespace, name))
}

fn text(node: Node) -> Option<String> {
    let text: String = node.descendants().filter(Node::is_text).filter_map(|t| t.text()).collect();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn status_code(node: Node) -> Option<u16> {
    // "HTTP/1.1 404 Not Found"
    text(node)?.split_whitespace().nth(1)?.parse().ok()
}

fn href_in(node: Node) -> Option<String> {
    child(node, DAV, "href").and_then(text)
}

/// Reads a 207 answer.
pub fn parse_multistatus(body: &str) -> Result<Multistatus, DavError> {
    let doc = Document::parse_with_options(body, options()).map_err(|err| DavError::Parse(err.to_string()))?;
    let root = doc.root_element();
    if !is(root, DAV, "multistatus") {
        return Err(DavError::Parse(format!("expected a multistatus, found {}", root.tag_name().name())));
    }
    let mut out = Multistatus::default();
    for node in root.children().filter(Node::is_element) {
        if is(node, DAV, "response") {
            out.responses.push(response(node));
        } else if is(node, DAV, "sync-token") {
            out.sync_token = text(node);
        }
    }
    Ok(out)
}

fn response(node: Node) -> DavResponse {
    let mut read = DavResponse {
        href: href_in(node).unwrap_or_default(),
        status: child(node, DAV, "status").and_then(status_code),
        props: Props::default(),
    };
    for propstat in node.children().filter(|c| is(*c, DAV, "propstat")) {
        let ok = child(propstat, DAV, "status").and_then(status_code).is_some_and(|s| (200..300).contains(&s));
        if let (true, Some(prop)) = (ok, child(propstat, DAV, "prop")) {
            read_props(prop, &mut read.props);
        }
    }
    read
}

fn read_props(prop: Node, props: &mut Props) {
    for p in prop.children().filter(Node::is_element) {
        let (ns, name) = (p.tag_name().namespace().unwrap_or_default(), p.tag_name().name());
        match (ns, name) {
            (DAV, "displayname") => props.displayname = text(p),
            (APPLE, "calendar-color") => props.color = text(p),
            (DAV, "resourcetype") => {
                props.is_calendar = child(p, CALDAV, "calendar").is_some();
                props.is_addressbook = child(p, CARDDAV, "addressbook").is_some();
            }
            (CALDAV, "supported-calendar-component-set") => {
                props.components = p
                    .children()
                    .filter(|c| is(*c, CALDAV, "comp"))
                    .filter_map(|c| c.attribute("name").map(str::to_ascii_uppercase))
                    .collect();
            }
            (DAV, "current-user-privilege-set") => {
                props.can_write = p.descendants().any(|d| {
                    d.is_element()
                        && d.tag_name().namespace() == Some(DAV)
                        && matches!(d.tag_name().name(), "write" | "write-content" | "all")
                });
            }
            (CS, "getctag") => props.getctag = text(p),
            (DAV, "sync-token") => props.sync_token = text(p),
            (DAV, "getetag") => props.etag = text(p),
            (CALDAV, "calendar-data") => props.calendar_data = text(p),
            (CARDDAV, "address-data") => props.address_data = text(p),
            (DAV, "current-user-principal") => props.principal = href_in(p),
            (CALDAV, "calendar-home-set") => props.calendar_home = href_in(p),
            (CARDDAV, "addressbook-home-set") => props.addressbook_home = href_in(p),
            (CALDAV, "calendar-timezone") => props.timezone = text(p),
            (DAV, "supported-report-set") => {
                props.reports_sync = p.descendants().any(|d| is(d, DAV, "sync-collection"));
            }
            _ => {}
        }
    }
}

/// The precondition a 403 or 409 body names, such as `valid-sync-token`.
pub fn precondition(body: &str) -> Option<String> {
    let doc = Document::parse_with_options(body, options()).ok()?;
    let root = doc.root_element();
    if !is(root, DAV, "error") {
        return None;
    }
    root.children().find(Node::is_element).map(|c| c.tag_name().name().to_string())
}

/// Text escaped for an XML element.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The request bodies.
pub mod body {
    use super::escape;
    use mailrs_domain::EpochMillis;

    pub fn principal() -> &'static str {
        r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:"><D:prop><D:current-user-principal/></D:prop></D:propfind>"#
    }

    pub fn homes() -> &'static str {
        r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav" xmlns:CR="urn:ietf:params:xml:ns:carddav">
<D:prop><C:calendar-home-set/><CR:addressbook-home-set/></D:prop></D:propfind>"#
    }

    pub fn collections() -> &'static str {
        r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav" xmlns:CS="http://calendarserver.org/ns/" xmlns:A="http://apple.com/ns/ical/">
<D:prop><D:resourcetype/><D:displayname/><A:calendar-color/><C:supported-calendar-component-set/>
<D:current-user-privilege-set/><C:calendar-timezone/><D:supported-report-set/><CS:getctag/><D:sync-token/></D:prop></D:propfind>"#
    }

    pub fn state() -> &'static str {
        r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:" xmlns:CS="http://calendarserver.org/ns/"><D:prop><D:sync-token/><CS:getctag/></D:prop></D:propfind>"#
    }

    /// Every member's etag, for an address book (Depth: 1).
    pub fn members() -> &'static str {
        r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:"><D:prop><D:getetag/><D:resourcetype/></D:prop></D:propfind>"#
    }

    /// RFC 6578; an empty token asks for every member.
    pub fn sync_collection(token: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<D:sync-collection xmlns:D="DAV:"><D:sync-token>{}</D:sync-token><D:sync-level>1</D:sync-level>
<D:prop><D:getetag/></D:prop></D:sync-collection>"#,
            escape(token)
        )
    }

    /// Every event's etag, those overlapping `range` when given. An end
    /// that is not a date (`i64::MAX`) leaves the range open.
    pub fn calendar_query(range: Option<(EpochMillis, EpochMillis)>) -> String {
        let stamp = |at: EpochMillis| chrono::DateTime::from_timestamp_millis(at).map(|at| at.format("%Y%m%dT%H%M%SZ").to_string());
        let range = range
            .and_then(|(from, to)| {
                let start = format!(r#" start="{}""#, stamp(from)?);
                let end = stamp(to).map(|end| format!(r#" end="{end}""#)).unwrap_or_default();
                Some(format!("<C:time-range{start}{end}/>"))
            })
            .unwrap_or_default();
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<C:calendar-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"><D:prop><D:getetag/></D:prop>
<C:filter><C:comp-filter name="VCALENDAR"><C:comp-filter name="VEVENT">{range}</C:comp-filter></C:comp-filter></C:filter>
</C:calendar-query>"#
        )
    }

    /// The events whose UID is `uid`, with their data.
    pub fn uid_query(uid: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<C:calendar-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"><D:prop><D:getetag/><C:calendar-data/></D:prop>
<C:filter><C:comp-filter name="VCALENDAR"><C:comp-filter name="VEVENT">
<C:prop-filter name="UID"><C:text-match collation="i;octet">{}</C:text-match></C:prop-filter>
</C:comp-filter></C:comp-filter></C:filter></C:calendar-query>"#,
            escape(uid)
        )
    }

    pub fn calendar_multiget(hrefs: &[String]) -> String {
        multiget("C:calendar-multiget", r#"xmlns:C="urn:ietf:params:xml:ns:caldav""#, "<C:calendar-data/>", hrefs)
    }

    pub fn addressbook_multiget(hrefs: &[String]) -> String {
        multiget("CR:addressbook-multiget", r#"xmlns:CR="urn:ietf:params:xml:ns:carddav""#, "<CR:address-data/>", hrefs)
    }

    fn multiget(element: &str, namespace: &str, data: &str, hrefs: &[String]) -> String {
        let listed: String = hrefs.iter().map(|h| format!("<D:href>{}</D:href>", escape(h))).collect();
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<{element} xmlns:D="DAV:" {namespace}><D:prop><D:getetag/>{data}</D:prop>{listed}</{element}>"#
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SYNC: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<multistatus xmlns="DAV:">
  <response><href>/me/work/a.ics</href>
    <propstat><prop><getetag>"1"</getetag></prop><status>HTTP/1.1 200 OK</status></propstat>
  </response>
  <response><href>/me/work/gone.ics</href><status>HTTP/1.1 404 Not Found</status></response>
  <sync-token>http://radicale.org/ns/sync/abc</sync-token>
</multistatus>"#;

    #[test]
    fn a_sync_collection_answer_lists_changes_removals_and_the_token() {
        let read = parse_multistatus(SYNC).unwrap();
        assert_eq!(read.sync_token.as_deref(), Some("http://radicale.org/ns/sync/abc"));
        assert_eq!(read.responses[0].props.etag.as_deref(), Some("\"1\""));
        assert_eq!(read.responses[1].status, Some(404));
    }

    const COLLECTIONS: &str = r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"
    xmlns:cs="http://calendarserver.org/ns/" xmlns:a="http://apple.com/ns/ical/">
  <d:response><d:href>/dav/calendars/user/me@fastmail.com/Default/</d:href>
    <d:propstat><d:prop>
      <d:displayname>Personal</d:displayname>
      <a:calendar-color>#3A87ADFF</a:calendar-color>
      <d:resourcetype><d:collection/><c:calendar/></d:resourcetype>
      <c:supported-calendar-component-set><c:comp name="VEVENT"/><c:comp name="VTODO"/></c:supported-calendar-component-set>
      <d:current-user-privilege-set><d:privilege><d:read/></d:privilege><d:privilege><d:write/></d:privilege></d:current-user-privilege-set>
      <cs:getctag>ctag-9</cs:getctag>
      <d:supported-report-set><d:supported-report><d:report><d:sync-collection/></d:report></d:supported-report></d:supported-report-set>
    </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
    <d:propstat><d:prop><c:calendar-timezone/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat>
  </d:response>
</d:multistatus>"#;

    #[test]
    fn a_collection_reads_its_name_colour_components_and_rights() {
        let read = parse_multistatus(COLLECTIONS).unwrap();
        let props = &read.responses[0].props;
        assert_eq!(props.displayname.as_deref(), Some("Personal"));
        assert_eq!(props.color.as_deref(), Some("#3A87ADFF"));
        assert!(props.is_calendar && !props.is_addressbook);
        assert_eq!(props.components, ["VEVENT", "VTODO"]);
        assert!(props.can_write);
        assert!(props.reports_sync);
        assert_eq!(props.getctag.as_deref(), Some("ctag-9"));
        assert_eq!(props.timezone, None, "a property the server did not have stays empty");
    }

    #[test]
    fn a_refused_token_names_its_precondition() {
        let body = r#"<D:error xmlns:D="DAV:"><D:valid-sync-token/></D:error>"#;
        assert_eq!(precondition(body).as_deref(), Some("valid-sync-token"));
    }

    #[test]
    fn a_document_type_declaration_is_refused() {
        let body = r#"<?xml version="1.0"?><!DOCTYPE x [<!ENTITY a "aaaa">]><multistatus xmlns="DAV:"/>"#;
        assert!(parse_multistatus(body).is_err());
    }

    #[test]
    fn an_open_ended_range_has_a_start_only() {
        let q = body::calendar_query(Some((0, i64::MAX)));
        assert!(q.contains(r#"<C:time-range start="19700101T000000Z"/>"#), "{q}");
    }

    #[test]
    fn the_sync_request_carries_its_token_escaped() {
        assert!(body::sync_collection("a&b").contains("<D:sync-token>a&amp;b</D:sync-token>"));
    }
}
