//! Penguin Mail's id for a calendar resource, and back. An event's id is
//! its resource's name without `.ics`, which is what the copy and an
//! occurrence id (`<id>_<start>`) carry; a name without `.ics` keeps its
//! whole self behind a `~`, so the way back is never a guess. A name that
//! ends in `_` and eight or sixteen digits would read as an occurrence
//! id; servers name resources by UUID or UID, which never do.

/// The id of the resource at `href`.
pub fn resource_id(href: &str) -> String {
    let segment = path_of(href).trim_end_matches('/').rsplit('/').next().unwrap_or_default().to_string();
    match segment.strip_suffix(".ics") {
        Some(stem) if !stem.is_empty() && !stem.starts_with('~') => stem.to_string(),
        _ => format!("~{segment}"),
    }
}

/// The href of resource `id` in `collection`.
pub fn resource_href(collection: &str, id: &str) -> String {
    match id.strip_prefix('~') {
        Some(segment) => join(collection, segment),
        None => join(collection, &format!("{id}.ics")),
    }
}

/// `href` as a path: a server may answer a full URL where it was asked a
/// path, and both name the same resource.
pub fn path_of(href: &str) -> String {
    match href.split_once("://") {
        Some((_, rest)) => match rest.find('/') {
            Some(at) => rest[at..].to_string(),
            None => "/".to_string(),
        },
        None => href.to_string(),
    }
}

/// `segment` under `collection`, with one slash between.
pub fn join(collection: &str, segment: &str) -> String {
    format!("{}/{}", collection.trim_end_matches('/'), segment.trim_start_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ics_resource_is_named_by_its_stem() {
        assert_eq!(resource_id("/dav/cal/work/abc-123.ics"), "abc-123");
        assert_eq!(resource_href("/dav/cal/work/", "abc-123"), "/dav/cal/work/abc-123.ics");
    }

    #[test]
    fn a_resource_without_ics_keeps_its_whole_name_behind_a_tilde() {
        assert_eq!(resource_id("/dav/cal/work/abc"), "~abc");
        assert_eq!(resource_href("/dav/cal/work", "~abc"), "/dav/cal/work/abc");
    }

    #[test]
    fn a_full_url_and_its_path_name_one_resource() {
        assert_eq!(path_of("https://p12-caldav.icloud.com/1234/calendars/home/"), "/1234/calendars/home/");
        assert_eq!(path_of("/1234/calendars/home/"), "/1234/calendars/home/");
    }
}
