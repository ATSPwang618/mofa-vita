use tjs_core::string;

fn legacy(units: &[u16]) -> Vec<u16> {
    let mut out = Vec::new();
    let mut hex = false;
    for &u in units.iter().take_while(|&&u| u != 0) {
        let escaped = match u {
            7 => Some(b'a'),
            8 => Some(b'b'),
            12 => Some(b'f'),
            10 => Some(b'n'),
            13 => Some(b'r'),
            9 => Some(b't'),
            11 => Some(b'v'),
            34 | 39 | 92 => Some(u as u8),
            _ => None,
        };
        if let Some(c) = escaped {
            out.extend([92, u16::from(c)]);
            hex = false;
        } else if u < 32 || (hex && matches!(u, 48..=57|65..=70|97..=102)) {
            const DIGITS: &[u8; 16] = b"0123456789abcdef";
            out.extend([
                92,
                120,
                u16::from(DIGITS[(u >> 4) as usize]),
                u16::from(DIGITS[(u & 15) as usize]),
            ]);
            hex = true;
        } else {
            out.push(u);
            hex = false;
        }
    }
    out
}

#[test]
fn escaping_runs_preserve_every_utf16_unit_and_hex_boundaries() {
    for unit in 0..=u16::MAX {
        for input in [
            vec![65, unit, 66, 0, 67],
            vec![1, unit, 49, 97, 102, 71, 39, 92, 0, 68],
        ] {
            assert_eq!(string::escape(&input), legacy(&input), "{input:?}");
            let mut appended = vec![1, 2, 3];
            string::escape_into(&input, &mut appended);
            assert_eq!(&appended[3..], legacy(&input));
        }
    }
}
