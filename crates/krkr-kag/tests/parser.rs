use krkr_kag::{Parser, Scenario, Token, units};
use std::sync::Arc;

fn parser(source: &str) -> Parser {
    let mut parser = Parser::default();
    parser.load(
        units("story.ks"),
        Arc::new(Scenario::new(units(source)).unwrap()),
    );
    parser
}

#[test]
fn nearest_call_label_matches_linear_history_at_every_position() {
    let mut p = parser("intro\n*first\na\nb\n*first\nc\n*|last\nd\n");
    for at in 0..=p.scenario.as_ref().unwrap().line_count() {
        p.position.line = at;
        let scenario = p.scenario.as_ref().unwrap();
        let labels = scenario.labels().unwrap();
        let previous = (0..at).rev().find(|&i| !labels.aliases[i].is_empty());
        let frame = p.call_frame().unwrap();
        assert_eq!(
            frame.label,
            previous.map_or_else(Vec::new, |i| labels.aliases[i].clone())
        );
        assert_eq!(frame.offset, at - previous.unwrap_or(0));
    }
}

#[test]
fn expansions_keep_saved_buffers_immutable_and_escape_brackets() {
    for replacement in ["abc", "a[b", "", "世界"] {
        let mut p = parser("left[x]right");
        let tag = loop {
            if let Token::Tag(tag) = p.next_token().unwrap() {
                break tag;
            }
        };
        p.expand(&tag, &units(replacement), true).unwrap();
        let snapshot = p.position.buffer.clone().unwrap();
        let expected = format!("left{}right", replacement.replace('[', "[["));
        assert_eq!(p.line(), units(&expected));
        let next = krkr_kag::Tag {
            start: 0,
            end: 4,
            ..tag
        };
        p.expand(&next, &units("new"), false).unwrap();
        assert_eq!(&**snapshot, &units(&expected));
        assert_eq!(
            p.line(),
            units(&format!("new{}right", replacement.replace('[', "[[")))
        );
    }
}

#[test]
fn labels_keep_case_duplicates_omitted_names_and_line_offsets() {
    let source = Scenario::new(units("\t*First|title\r\nbody\r*|again\n*First\n*first\n")).unwrap();
    let labels = source.labels().unwrap();
    for (name, line) in [
        ("*First", 0),
        ("*First:2", 2),
        ("*First:3", 3),
        ("*first", 4),
    ] {
        assert_eq!(labels.by_name[&units(name)], line);
    }
    assert_eq!(source.line_count(), 5);
    assert!(
        Scenario::new(units("*|missing\nx"))
            .unwrap()
            .labels()
            .is_err()
    );
}

#[test]
fn line_rules_interrupt_and_ignore_cr_do_not_change_literal_spaces() {
    let mut p = parser("\t;comment\n \tA\\\n[[B[p]\nC\n");
    p.interrupted = true;
    assert!(matches!(p.next_token().unwrap(), Token::Interrupt));
    let mut text = Vec::new();
    let mut tags = Vec::new();
    loop {
        match p.next_token().unwrap() {
            Token::Character(c) => text.push(c),
            Token::Newline(_) => text.push(10),
            Token::Tag(t) => {
                tags.push(t.name.clone());
                p.finish_tag(&t);
            }
            Token::End => break,
            Token::Skip => {}
            _ => panic!("unexpected token"),
        }
    }
    assert_eq!(text, units(" A[BC\n"));
    assert_eq!(tags, vec![units("p")]);
    p = parser("A\\\nB");
    p.ignore_cr = true;
    assert!(matches!(p.next_token().unwrap(), Token::Character(65)));
    assert!(matches!(p.next_token().unwrap(), Token::Character(92)));
}

#[test]
fn call_positions_detect_modified_source_and_expansion_is_copy_on_write() {
    let mut p = parser("*start\n[macro]after");
    p.goto(&units("*start")).unwrap();
    p.advance_line();
    let Token::Tag(tag) = p.next_token().unwrap() else {
        panic!("tag")
    };
    p.expand(&tag, &units("inside"), false).unwrap();
    let saved = p.clone();
    p.push_call().unwrap();
    let frame = p.calls[0].clone();
    p.load(
        units("story.ks"),
        Arc::new(Scenario::new(units("*start\nchanged")).unwrap()),
    );
    assert!(p.return_position(&frame).is_err());
    assert_eq!(saved.line(), units("insideafter"));
}
