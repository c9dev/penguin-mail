use mailrs_mime::charset::charset_param;

#[test]
fn charset_parameter_parsing() {
    assert_eq!(
        charset_param(r#"text/plain; format=flowed; charset="utf-8""#),
        Some("utf-8")
    );
    assert_eq!(
        charset_param("text/plain; CHARSET=windows-1252"),
        Some("windows-1252")
    );
    assert_eq!(charset_param("text/plain"), None);
}
