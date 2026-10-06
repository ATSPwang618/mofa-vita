use tjs_core::{Heap, Value, octet};

#[test]
fn array_pack_borrows_values_and_preserves_inline_and_spilled_templates() {
    let mut heap = Heap::new();
    let text = Value::Str(heap.alloc_string([65, 0, 66]));
    let input = [Value::Int(-1), text, Value::Int(0x12345678)];
    let array = heap.alloc_array_from(&input).unwrap();
    for format in ["Ca*N", "C0C0C0C0C0C0C0C0Ca*N", "C0C0C0C0C0C0C0C0"] {
        let format = Value::Str(heap.alloc_string(format.encode_utf16().collect::<Vec<_>>()));
        let expected = octet::pack(&mut heap, &input, format).unwrap();
        let actual = octet::pack_array(&mut heap, array, format).unwrap();
        match (actual, expected) {
            (Value::Octet(a), Value::Octet(b)) => {
                assert_eq!(heap.octet(a).unwrap(), heap.octet(b).unwrap())
            }
            (Value::Obj(a), Value::Obj(b)) => assert_eq!(a, b),
            _ => panic!("different pack results"),
        }
        assert_eq!(heap.array(array).unwrap().len(), input.len());
        assert_eq!(heap.array(array).unwrap()[0].as_integer(), Some(-1));
        let Value::Str(id) = heap.array(array).unwrap()[1] else {
            panic!("string input")
        };
        assert_eq!(heap.string(id).unwrap(), [65, 0, 66]);
    }
}

#[test]
fn unpack_keeps_input_live_across_string_allocations_and_repositioning() {
    let mut heap = Heap::new();
    let source = heap.alloc_octet([1, 2, b'a', b'b', b'c', 255]);
    let format = Value::Str(heap.alloc_string("C2a3X2C2".encode_utf16().collect::<Vec<_>>()));
    let result = octet::unpack(&mut heap, source, format).unwrap();
    let Value::Obj(array) = result else {
        panic!("array result")
    };
    let values = heap.array(array.object.unwrap()).unwrap();
    assert_eq!(values.len(), 5);
    assert_eq!(values[0].as_integer(), Some(1));
    assert_eq!(values[1].as_integer(), Some(2));
    let Value::Str(text) = values[2] else {
        panic!("string field")
    };
    assert_eq!(heap.string(text).unwrap(), [97, 98, 99]);
    assert_eq!(values[3].as_integer(), Some(98));
    assert_eq!(values[4].as_integer(), Some(99));
    assert_eq!(heap.octet(source).unwrap(), [1, 2, 97, 98, 99, 255]);
    heap.collect([result]);
    assert_eq!(heap.string(text).unwrap(), [97, 98, 99]);
}
