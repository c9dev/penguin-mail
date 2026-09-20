use mailrs_store::open_in_memory;
use mailrs_store::templates::{self, Template};

fn template(name: &str) -> Template {
    Template {
        id: 0,
        name: name.into(),
        subject: format!("About {name}"),
        markdown: format!("Hello from **{name}**."),
    }
}

#[test]
fn templates_come_back_in_name_order() {
    let conn = open_in_memory().unwrap();
    templates::add(&conn, &template("Thanks")).unwrap();
    templates::add(&conn, &template("Away")).unwrap();
    templates::add(&conn, &template("booking")).unwrap();
    let names: Vec<String> = templates::list(&conn)
        .unwrap()
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert_eq!(names, ["Away", "booking", "Thanks"]);
}

#[test]
fn a_template_keeps_its_subject_and_its_marks() {
    let conn = open_in_memory().unwrap();
    let id = templates::add(&conn, &template("Thanks")).unwrap();
    let saved = templates::list(&conn).unwrap().remove(0);
    assert_eq!(saved.id, id);
    assert_eq!(saved.subject, "About Thanks");
    assert_eq!(saved.markdown, "Hello from **Thanks**.");
}

#[test]
fn renaming_and_editing_change_the_same_row() {
    let conn = open_in_memory().unwrap();
    let id = templates::add(&conn, &template("Away")).unwrap();
    templates::update(
        &conn,
        &Template {
            id,
            name: "Out of Office".into(),
            subject: String::new(),
            markdown: "Back on Monday.".into(),
        },
    )
    .unwrap();
    let all = templates::list(&conn).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].name, "Out of Office");
    assert_eq!(all[0].subject, "");
    assert_eq!(all[0].markdown, "Back on Monday.");
}

#[test]
fn deleting_a_template_leaves_the_others() {
    let conn = open_in_memory().unwrap();
    let id = templates::add(&conn, &template("Away")).unwrap();
    templates::add(&conn, &template("Thanks")).unwrap();
    templates::remove(&conn, id).unwrap();
    let names: Vec<String> = templates::list(&conn)
        .unwrap()
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert_eq!(names, ["Thanks"]);
}
