//! Allocation-free token lookahead for the original tjsdate.y grammar.
use super::*;
use crate::Heap;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Token {
    Number(i32),
    Month(i32),
    Weekday,
    Zone(i32),
    AmPm(bool),
    Punct(u16),
    End,
}

pub(super) fn parse(heap: &Heap, value: Value) -> NativeResult<i64> {
    let Value::Str(id) = value else {
        return Err(NativeError::Type("a date string"));
    };
    parse_in(
        tjs_core::string::c_string(heap.string(id)?),
        &timezone::system(),
    )
}

fn parse_in(units: &[u16], zone: &timezone::Zone) -> NativeResult<i64> {
    let mut input = Input {
        units,
        pos: 0,
        lookahead: [None, None],
    };
    if input.peek() == Token::Weekday {
        input.take();
        input.punct(b',');
    }
    let (mut year, month, day);
    if let Some(mon) = input.month() {
        month = mon;
        let hyphen = input.punct(b'-');
        day = input.number()?;
        let year_hyphen = input.punct(b'-');
        if year_hyphen && (!hyphen || input.time_ahead()) {
            return Err(bad());
        }
        year = if input.time_ahead() {
            None
        } else {
            Some(input.number()?)
        };
    } else {
        let first = input.number()?;
        if matches!(input.peek(), Token::Punct(45 | 47))
            && matches!(input.second(), Token::Number(_))
        {
            input.take();
            month = input.number()?.wrapping_sub(1);
            if !input.punct(b'-') && !input.punct(b'/') {
                return Err(bad());
            }
            day = input.number()?;
            year = Some(first);
        } else {
            day = first;
            let hyphen = input.punct(b'-');
            month = input.month().ok_or_else(bad)?;
            let year_hyphen = input.punct(b'-');
            if year_hyphen && (!hyphen || input.time_ahead()) {
                return Err(bad());
            }
            year = if input.time_ahead() {
                None
            } else {
                Some(input.number()?)
            };
        }
    }
    let before = input.ampm();
    let mut hour = input.number()?;
    if !input.punct(b':') {
        return Err(bad());
    }
    let minute = input.number()?;
    let second = if input.punct(b':') {
        let value = input.number()?;
        if input.punct(b'.') {
            input.number()?;
        }
        value
    } else {
        0
    };
    let after = input.ampm();
    if before.is_some() && after.is_some() {
        return Err(bad());
    }
    if before.or(after) == Some(true) {
        hour = hour.wrapping_add(12);
    }
    if year.is_none() {
        year = Some(input.number()?);
    }
    let mut year = year.expect("required year");
    if year < 100 {
        year = year.wrapping_add(if year <= 50 { 2000 } else { 1900 });
    }

    let named = if let Token::Zone(offset) = input.peek() {
        input.take();
        Some(offset)
    } else {
        None
    };
    let sign = if input.punct(b'+') {
        1i32
    } else if input.punct(b'-') {
        -1
    } else {
        0
    };
    let offset = if sign != 0 {
        Some(offset_seconds(input.number()?.wrapping_mul(sign)))
    } else {
        None
    };
    if input.punct(b'(') {
        // The grammar admits a single description only after the whole date,
        // time and offset. Its arbitrary contents are never tokenized.
        while input.units.get(input.pos).is_some_and(|&c| c != 41) {
            input.pos += 1;
        }
        input.lookahead = [None, None];
        if !input.punct(b')') {
            return Err(bad());
        }
    }
    if input.peek() != Token::End {
        return Err(bad());
    }
    let date = normalize([year, month, day, hour, minute, second])?;
    let mut seconds = local_timestamp_in(zone, date, Dst::No)?;
    if named.is_some() || offset.is_some() {
        // The reference runs mktime with tm_isdst=0 before subtracting the
        // process standard timezone and the explicit parsed offset.
        let standard = zone.standard_offset(Timestamp::now())?.seconds();
        let parsed = named.unwrap_or(0).wrapping_add(offset.unwrap_or(0));
        seconds += i64::from(standard) - i64::from(parsed);
    }
    Timestamp::from_second(seconds).map_err(error)?;
    Ok(seconds)
}

fn offset_seconds(mut hhmm: i32) -> i32 {
    let negative = hhmm < 0;
    if negative {
        hhmm = hhmm.wrapping_neg();
    }
    let seconds = (hhmm / 100)
        .wrapping_mul(3600)
        .wrapping_add((hhmm % 100).wrapping_mul(60));
    if negative {
        seconds.wrapping_neg()
    } else {
        seconds
    }
}

struct Input<'a> {
    units: &'a [u16],
    pos: usize,
    lookahead: [Option<Token>; 2],
}
impl Input<'_> {
    fn lex(&mut self) -> Token {
        while self
            .units
            .get(self.pos)
            .is_some_and(|c| matches!(c, 9..=13 | 32))
        {
            self.pos += 1;
        }
        let Some(&ch) = self.units.get(self.pos) else {
            return Token::End;
        };
        if matches!(ch, 48..=57) {
            let mut n = 0i32;
            while let Some(&c @ 48..=57) = self.units.get(self.pos) {
                n = n.wrapping_mul(10).wrapping_add(i32::from(c - 48));
                self.pos += 1;
            }
            return Token::Number(n);
        }
        if matches!(ch,65..=90|97..=122) {
            let mut word = [0u8; 10];
            let mut length = 0;
            while let Some(&c @ (65..=90 | 97..=122)) = self.units.get(self.pos) {
                if length == word.len() {
                    return Token::Punct(ch);
                }
                word[length] = (c as u8).to_ascii_lowercase();
                length += 1;
                self.pos += 1;
            }
            if self.units.get(self.pos) == Some(&46) {
                if length == word.len() {
                    return Token::Punct(ch);
                }
                word[length] = b'.';
                length += 1;
                self.pos += 1;
            }
            let word = std::str::from_utf8(&word[..length]).expect("ASCII word");
            return if let Some(month) = month_index(word) {
                Token::Month(month)
            } else if weekday(word) {
                Token::Weekday
            } else if let Some(zone) = zone_offset(word) {
                Token::Zone(zone)
            } else if word == "am" {
                Token::AmPm(false)
            } else if word == "pm" {
                Token::AmPm(true)
            } else {
                Token::Punct(ch)
            };
        }
        self.pos += 1;
        Token::Punct(ch)
    }
    fn peek(&mut self) -> Token {
        if self.lookahead[0].is_none() {
            self.lookahead[0] = Some(self.lex());
        }
        self.lookahead[0].unwrap()
    }
    fn second(&mut self) -> Token {
        self.peek();
        if self.lookahead[1].is_none() {
            self.lookahead[1] = Some(self.lex());
        }
        self.lookahead[1].unwrap()
    }
    fn take(&mut self) -> Token {
        let token = self.peek();
        self.lookahead[0] = self.lookahead[1].take();
        token
    }
    fn punct(&mut self, c: u8) -> bool {
        if self.peek() == Token::Punct(u16::from(c)) {
            self.take();
            true
        } else {
            false
        }
    }
    fn number(&mut self) -> NativeResult<i32> {
        if let Token::Number(value) = self.take() {
            Ok(value)
        } else {
            Err(bad())
        }
    }
    fn month(&mut self) -> Option<i32> {
        if let Token::Month(value) = self.peek() {
            self.take();
            Some(value)
        } else {
            None
        }
    }
    fn ampm(&mut self) -> Option<bool> {
        if let Token::AmPm(value) = self.peek() {
            self.take();
            Some(value)
        } else {
            None
        }
    }
    fn time_ahead(&mut self) -> bool {
        matches!(self.peek(), Token::AmPm(_)) || self.second() == Token::Punct(58)
    }
}

fn month_index(word: &str) -> Option<i32> {
    Some(match word {
        "jan" | "jan." | "january" => 0,
        "feb" | "feb." | "february" => 1,
        "mar" | "mar." | "march" => 2,
        "apr" | "apr." | "april" => 3,
        "may" => 4,
        "ju" | "ju." | "jun" | "jun." | "june" => 5,
        "jul" | "jul." | "july" => 6,
        "aug" | "aug." | "august" => 7,
        "sep" | "sep." | "sept" | "sept." | "september" => 8,
        "oct" | "oct." | "october" => 9,
        "nov" | "nov." | "november" => 10,
        "dec" | "dec." | "december" => 11,
        _ => return None,
    })
}
// Fixed historical abbreviations from krkrz/tjs2/syntax/dp_wordtable.txt.
// They are language tokens; their meaning does not follow modern zone naming.
fn weekday(word: &str) -> bool {
    matches!(
        word,
        "sun"
            | "sun."
            | "sunday"
            | "mon"
            | "mon."
            | "monday"
            | "tue"
            | "tue."
            | "tues"
            | "tues."
            | "tuesday"
            | "wed"
            | "wed."
            | "wednesday"
            | "thu"
            | "thu."
            | "thurs"
            | "thurs."
            | "thursday"
            | "fri"
            | "fri."
            | "friday"
            | "sat"
            | "sat."
            | "saturday"
    )
}
fn zone_offset(word: &str) -> Option<i32> {
    let hhmm: i32 = match word {
        "ut" | "utc" | "gmt" | "z" | "wet" => 0,
        "a" | "wat" => -100,
        "m" | "idlw" => -1200,
        "n" => 100,
        "y" | "idle" | "nzst" | "nzt" => 1200,
        "nzdt" => 1300,
        "aesst" => 1100,
        "acsst" | "cadt" | "sadt" => 1030,
        "aest" | "east" | "gst" | "ligt" => 1000,
        "acst" | "sast" | "cast" => 930,
        "jst" | "awsst" | "kst" | "wdt" => 900,
        "mt" => 830,
        "awst" | "cct" | "wadt" | "wst" => 800,
        "jt" => 730,
        "wast" => 700,
        "it" => 330,
        "bt" | "eetdst" => 300,
        "cetdst" | "eet" | "fwt" | "ist" | "mest" | "metdst" | "sst" => 200,
        "bst" | "cet" | "dnt" | "fst" | "met" | "mewt" | "mez" | "nor" | "set" | "swt"
        | "wetdst" => 100,
        "ndt" => -230,
        "adt" => -300,
        "nft" | "nst" => -330,
        "edt" | "ast" => -400,
        "est" | "cdt" => -500,
        "cst" | "mdt" => -600,
        "mst" | "pdt" => -700,
        "pst" | "ydt" => -800,
        "hdt" => -900,
        "ahst" | "cat" => -1000,
        "nt" => -1100,
        _ => return None,
    };
    Some((hhmm / 100) * 3600 + (hhmm % 100) * 60)
}
fn bad() -> NativeError {
    NativeError::Message("cannot parse date")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(text: &str) -> NativeResult<i64> {
        parse_in(
            &text.encode_utf16().collect::<Vec<_>>(),
            &TimeZone::UTC.into(),
        )
    }
    #[test]
    fn original_grammar_and_word_boundaries() {
        for text in [
            "Thu, 1 Jan 1970 00:00 GMT",
            "1-Jan 1970 0:0:0",
            "1-Jan.-1970 0:0:0.123",
            "January 1 1970 0:0",
            "Jan-1 1970 0:0",
            "Jan-1-1970 0:0",
            "1 January 0:0 1970",
            "1-Jan 0:0 1970",
            "Jan 1 0:0 1970",
            "Jan-1 0:0 1970",
            "1970/1-1 0:0",
            "Thu.1970-1/1 am 0:0 UTC +0000 (说明)",
        ] {
            assert_eq!(parse(text).unwrap(), 0, "{text}");
        }
        for text in [
            "1 Jan-1970 0:0",
            "Jan 1-1970 0:0",
            "Jan.-1-0:0 1970",
            "1970/1/1 0:0 am pm",
            "1970/1/1 0:0.1",
            "1970/1/1 (note) 0:0",
            "1970/1/1 0:0 (note) GMT",
            "1970/1/1 0:0 GMT (note) (more)",
            "1970/1/1 0:0 GMT (open",
            "Janx 1 1970 0:0",
            "January. 1 1970 0:0",
            "1970/1/1 0:0 UTC.",
            "1970/1/1 0:0 +08:00",
            "1970/1/1T0:0Z",
        ] {
            assert!(parse(text).is_err(), "{text}");
        }
        assert_eq!(parse("4294967295/1/1 0:0 GMT").unwrap(), 915148800);
        assert_eq!(parse("1970/1/1 0:0 +2147483647").unwrap(), -1092);
        assert_eq!(parse("1970/1/1 0:0 -2147483647").unwrap(), 1092);
        assert!(parse("1969/12/31 23:59:59 GMT").is_err());
        assert_eq!(parse("1970/1/1 0:1:59 GMT +0002").unwrap(), -1);
    }
    #[test]
    fn historical_zone_offsets_and_additive_ampm() {
        for (name, seconds) in [
            ("ACST", 34200),
            ("JST", 32400),
            ("NST", -12600),
            ("A", -3600),
            ("N", 3600),
            ("NZDT", 46800),
        ] {
            assert_eq!(parse(&format!("1970/1/1 0:0 {name}")).unwrap(), -seconds);
        }
        assert_eq!(parse("1970/1/1 0:0 JST -0900").unwrap(), 0);
        assert_eq!(parse("1970/1/1 pm 12:00 GMT").unwrap(), 86400);
        assert_eq!(parse("1970/1/1 12:00 am GMT").unwrap(), 43200);
    }
}
