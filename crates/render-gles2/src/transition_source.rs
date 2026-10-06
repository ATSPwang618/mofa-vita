//! Shared by runtime specialization and the SGX binary build catalog.
pub(crate) fn fragment(rule: bool, face: u8, direct: bool) -> String {
    format!(
        "#define TRANSITION_RULE {}\n#define TRANSITION_FACE {}\n#define TRANSITION_DIRECT {}\n{}",
        u8::from(rule),
        face,
        u8::from(direct),
        include_str!("transition.frag")
    )
}

pub(crate) fn custom(mode: u8) -> String {
    if mode >= 16 {
        return format!(
            "#define NAGANO_MODE {}\n{}",
            mode - 16,
            include_str!("nagano.frag")
        );
    }
    format!(
        "#define EXTRANS_MODE {mode}\n{}",
        include_str!("extrans.frag")
    )
}
