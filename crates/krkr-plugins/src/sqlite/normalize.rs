//! The reference's Japanese search normalization table for cnt/ncnt.
use rusqlite::{Connection, functions::FunctionFlags, types::ValueRef};
const BEFORE: &str = r##"ABCDEFGHIJKLMNOPQRSTUVWXYZＡＢＣＤＥＦＧＨＩＪＫＬＭＮＯＰＱＲＳＴＵＶＷＸＹＺａｂｃｄｅｆｇｈｉｊｋｌｍｎｏｐｑｒｓｔｕｖｗｘｙｚ１２３４５６７８９０あいうえおかきくけこさしすせそたちつてとなにぬねのはひふへほまみむめもやゆよらりるれろわゐゑをんぁぃぅぇぉっゃゅょがぎぐげござじずぜぞだぢづでどばびぶべぼぱぴぷぺぽアイウエオカキクケコサシスセソタチツテトナニヌネノハヒフヘホマミムメモヤユヨラリルレロワヰヱヲンァィゥェォッャュョガギグゲゴザジズゼゾダヂヅデドバビブベボパピプペポｧｨｩｪｫｯｬｭｮー・、。ｰ[]{}，．：；？！´｀＾￣＿〇ー―‐／＼～｜‘’“”（）〔〕［］｛｝〈〉《》「」『』【】＋－×＝＜＞￥＄％＃＆＊＠★●◎◆■▲▼※"##;
const AFTER: &str = r##"abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz1234567890ｱｲｳｴｵｶｷｸｹｺｻｼｽｾｿﾀﾁﾂﾃﾄﾅﾆﾇﾈﾉﾊﾋﾌﾍﾎﾏﾐﾑﾒﾓﾔﾕﾖﾗﾘﾙﾚﾛﾜｲｴｦﾝｱｲｳｴｵﾂﾔﾕﾖｶｷｸｹｺｻｼｽｾｿﾀﾁﾂﾃﾄﾊﾋﾌﾍﾎﾊﾋﾌﾍﾎｱｲｳｴｵｶｷｸｹｺｻｼｽｾｿﾀﾁﾂﾃﾄﾅﾆﾇﾈﾉﾊﾋﾌﾍﾎﾏﾐﾑﾒﾓﾔﾕﾖﾗﾘﾙﾚﾛﾜｲｴｦﾝｱｲｳｴｵﾂﾔﾕﾖｶｷｸｹｺｻｼｽｾｿﾀﾁﾂﾃﾄﾊﾋﾌﾍﾎﾊﾋﾌﾍﾎｱｲｳｴｵﾂﾔﾕﾖ-･,.-()(),.:;?!'`^~_◯---/＼-|`'""()()()()()()｢｣｢｣()+-x=<>\$%#&*@☆○○◇□△▽*"##;
fn text(value: ValueRef<'_>) -> String {
    match value {
        ValueRef::Null => String::new(),
        ValueRef::Integer(n) => n.to_string(),
        ValueRef::Real(n) => n.to_string(),
        ValueRef::Text(s) | ValueRef::Blob(s) => String::from_utf8_lossy(s)
            .split('\0')
            .next()
            .unwrap_or("")
            .to_owned(),
    }
}
fn normalize(source: &str, table: &std::collections::BTreeMap<char, char>) -> String {
    source
        .chars()
        .filter(|c| !"ﾞ゛゜".contains(*c))
        .map(|c| table.get(&c).copied().unwrap_or(c))
        .collect()
}
pub(super) fn install(db: &Connection) -> rusqlite::Result<()> {
    db.create_scalar_function(
        "cnt",
        2,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |cx| Ok(text(cx.get_raw(0)).contains(&text(cx.get_raw(1)))),
    )?;
    let table: std::collections::BTreeMap<_, _> = BEFORE.chars().zip(AFTER.chars()).collect();
    db.create_scalar_function(
        "ncnt",
        2,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        move |cx| {
            Ok(normalize(&text(cx.get_raw(0)), &table)
                .contains(&normalize(&text(cx.get_raw(1)), &table)))
        },
    )?;
    Ok(())
}
