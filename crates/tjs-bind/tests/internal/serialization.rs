use super::*;

#[test]
fn binary_dictionary_keys_intern_without_changing_utf16_or_nul_rules() {
    for len in [0, 1, 31, 32, 255, 256, 65535, 65536] {
        for nul in [false, true] {
            let mut heap = Heap::new();
            let id = heap.alloc_dictionary();
            let mut units = vec![0xd800; len];
            if nul && len != 0 {
                units[len / 2] = 0;
            }
            let name = heap.intern(&units);
            heap.set_member(id, name, Value::Int(71)).unwrap();
            let encoded = binary::encode(&heap, id).unwrap();
            let target = heap.alloc_dictionary();
            binary::decode(&mut heap, &encoded, target).unwrap();
            let expected = heap.intern(tjs_core::string::c_string(&units));
            assert_eq!(
                heap.member(target, expected).unwrap().unwrap().as_integer(),
                Some(71)
            );
            heap.collect([Value::Obj(target.into())]);
            assert_eq!(
                heap.symbol(expected).unwrap(),
                tjs_core::string::c_string(&units)
            );
        }
    }
    for input in [
        &b"KBAD100\0\x81\xa1\x41"[..],
        &b"KBAD100\0\x81\x91\x01"[..],
        &b"KBAD100\0\x81\xc6\xff\xff\xff\xff"[..],
    ] {
        let mut heap = Heap::new();
        let target = heap.alloc_dictionary();
        assert!(binary::decode(&mut heap, input, target).is_err());
    }
}

#[test]
fn text_structure_keeps_escaping_integer_extremes_octets_and_recursion() {
    let mut heap = Heap::new();
    let array = heap.alloc_array();
    let string = heap.alloc_string([1, 65, 10, 34, 92, 0xd800, 0, 66]);
    let bytes = heap.alloc_octet([0, 15, 16, 127, 255]);
    for value in [
        Value::Str(string),
        Value::Int(i64::MIN),
        Value::Int(i64::MAX),
        Value::Octet(bytes),
        Value::Obj(array.into()),
    ] {
        heap.array_push(array, value).unwrap();
    }
    let output = text::encode(&mut heap, array).unwrap();
    let expected = ["(const) [\r\n \"\\x01\\x41\\n\\\"\\\\".encode_utf16().collect::<Vec<_>>(), vec![0xd800], "\",\r\n -9223372036854775808,\r\n 9223372036854775807,\r\n <%000f107fff%>,\r\n null /* object recursion detected */\r\n]".encode_utf16().collect()].concat();
    assert_eq!(output, expected);
}
