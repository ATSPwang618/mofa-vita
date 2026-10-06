pub struct Log;

impl krkr_engine::debug::LogOutput for Log {
    fn timestamp(&mut self) -> String {
        "00:00:00".into()
    }
    fn console(&mut self, line: &[u16]) {
        eprintln!("{}", String::from_utf16_lossy(line));
    }
}
