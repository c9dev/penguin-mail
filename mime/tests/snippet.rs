use mailrs_mime::snippet::unescape_snippet;

#[test]
fn snippet_entities_are_decoded() {
    assert_eq!(
        unescape_snippet("Tom &amp; Jerry&#39;s &lt;show&gt; &#x41; &bogus; & more"),
        "Tom & Jerry's <show> A &bogus; & more"
    );
}
