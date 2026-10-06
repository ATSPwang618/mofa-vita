use super::*;

#[test]
fn encoders_are_resolved_relative_to_converter_and_missing_tool_is_named() {
    let root = tempfile::tempdir().unwrap();
    let install = root.path().join("转换器 with spaces");
    std::fs::create_dir(&install).unwrap();
    let converter = install.join(format!("krkr-convert{}", std::env::consts::EXE_SUFFIX));
    let tool = install.join(format!("at9tool{}", std::env::consts::EXE_SUFFIX));
    // A tool elsewhere must not satisfy the converter's local dependency.
    std::fs::write(root.path().join(tool.file_name().unwrap()), b"unrelated").unwrap();
    let error = beside(&converter, "at9tool").unwrap_err();
    assert!(error.contains("at9tool"));
    assert!(error.contains(&tool.display().to_string()));
    std::fs::write(&tool, b"local").unwrap();
    assert_eq!(beside(&converter, "at9tool").unwrap(), tool);
}
