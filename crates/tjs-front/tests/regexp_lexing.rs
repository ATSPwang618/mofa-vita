use tjs_core::{Phase, SourceMap};
use tjs_front::{lexer::TokenKind, parser};

#[test]
fn parser_directed_tokens_roundtrip_and_report_original_regex_spans() {
    let mut sources = SourceMap::new();
    let script = r#"/// 日本語
var r=/["'}]/gi; var n=8; n/=2; n/r.match("}").count;"#;
    let id = sources.add_utf8("regex tokens", script).unwrap();
    let (_, tokens) = parser::parse_source(&sources, id).unwrap();
    let token = tokens
        .tokens()
        .iter()
        .find(|t| matches!(t.kind, TokenKind::RegExp(_)))
        .unwrap();
    assert_eq!(
        String::from_utf16(sources.slice(token.span).unwrap()).unwrap(),
        r#"/["'}]/gi"#
    );
    assert_eq!(
        tokens
            .trivia()
            .iter()
            .filter(|t| t.kind == tjs_front::lexer::TriviaKind::DocLine)
            .count(),
        1
    );
    assert!(
        tokens
            .tokens()
            .iter()
            .any(|t| t.kind == TokenKind::SlashEqual)
    );
    assert!(tokens.tokens().iter().any(|t| t.kind == TokenKind::Slash));
    parser::parse(&tokens).unwrap();
    for script in ["/unfinished", r#"var r=/escaped\/;"#] {
        let id = sources.add_utf8("bad regex", script).unwrap();
        let error = parser::parse_source(&sources, id).unwrap_err();
        assert_eq!(error.phase, Phase::Lex);
        assert!(
            String::from_utf16(sources.slice(error.span.unwrap()).unwrap())
                .unwrap()
                .starts_with('/')
        );
    }
}
