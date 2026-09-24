use mailrs_domain::Address;
use mailrs_mime::address::{parse_address_list, parse_address_list_keeping_invalid};

fn addr(name: Option<&str>, email: &str) -> Address {
    Address {
        name: name.map(str::to_string),
        email: email.into(),
    }
}

#[test]
fn parses_a_bare_address() {
    assert_eq!(
        parse_address_list("bob@example.com"),
        vec![addr(None, "bob@example.com")]
    );
}

#[test]
fn parses_a_named_address() {
    assert_eq!(
        parse_address_list("Ann Lee <ann@example.com>"),
        vec![addr(Some("Ann Lee"), "ann@example.com")]
    );
}

#[test]
fn quoted_names_may_contain_commas() {
    assert_eq!(
        parse_address_list(r#""Lee, Ann" <ann@example.com>, bob@example.com"#),
        vec![
            addr(Some("Lee, Ann"), "ann@example.com"),
            addr(None, "bob@example.com")
        ]
    );
}

#[test]
fn escaped_quotes_are_unescaped() {
    assert_eq!(
        parse_address_list(r#""Ann \"The Boss\" Lee" <ann@example.com>"#),
        vec![addr(Some(r#"Ann "The Boss" Lee"#), "ann@example.com")]
    );
}

#[test]
fn group_syntax_and_empty_input_yield_nothing() {
    assert!(parse_address_list("undisclosed-recipients:;").is_empty());
    assert!(parse_address_list("").is_empty());
}

#[test]
fn the_lenient_parser_keeps_mistakes() {
    assert_eq!(
        parse_address_list_keeping_invalid("ann@example.com, not an address, "),
        vec![addr(None, "ann@example.com"), addr(None, "not an address")]
    );
}
