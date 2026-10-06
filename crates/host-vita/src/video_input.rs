use std::io::{self, BufReader, Read, Seek, SeekFrom};
pub const CACHE_BYTES: usize = 64 * 1024;

/// Complete an SDK file request even when playback is being cancelled. AvPlayer
/// owns its streaming threads until Close returns; cancelling playback is not
/// a storage error and must not turn their outstanding reads into failures.
pub fn read_at<R: Read + Seek>(
    input: &mut BufReader<R>,
    position: u64,
    output: &mut [u8],
) -> io::Result<usize> {
    seek_to(input, position)?;
    let mut done = 0;
    while done < output.len() {
        match input.read(&mut output[done..]) {
            Ok(0) => break,
            Ok(count) => done += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(done)
}

/// BufReader::seek(Start) discards read-ahead even for an adjacent packet.
/// Relative seeks preserve cached bytes when AvPlayer alternates A/V offsets.
pub fn seek_to<R: Read + Seek>(input: &mut BufReader<R>, position: u64) -> io::Result<()> {
    let current = input.stream_position()?;
    if let Ok(delta) = i64::try_from(i128::from(position) - i128::from(current)) {
        input.seek_relative(delta)
    } else {
        input.seek(SeekFrom::Start(position)).map(|_| ())
    }
}
