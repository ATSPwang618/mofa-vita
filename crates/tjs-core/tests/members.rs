use tjs_core::{Heap, HeapCounts, Instruction, Module, Register, Value, member};

#[test]
fn member_lookup_does_not_intern_misses_or_allocate_on_repeated_updates() {
    let mut heap = Heap::new();
    let object = Value::Obj(heap.alloc_dictionary().into());
    let key = Value::Str(heap.alloc_string("name".encode_utf16().collect::<Vec<_>>()));
    let miss = Value::Str(heap.alloc_string("missing".encode_utf16().collect::<Vec<_>>()));
    for _ in 0..100 {
        assert!(matches!(
            member::get(&mut heap, object, miss).unwrap(),
            Value::Void
        ));
    }
    assert_eq!(heap.counts().symbols, 0);
    member::set(&mut heap, object, key, Value::Int(1)).unwrap();
    heap.collect([object, key, miss]);
    let before = heap.counts();
    for n in 0..100 {
        member::set(&mut heap, object, key, Value::Int(n)).unwrap();
        assert_eq!(
            member::get(&mut heap, object, key).unwrap().as_integer(),
            Some(n)
        );
        assert!(!member::delete(&mut heap, object, miss).unwrap());
    }
    assert_eq!(heap.counts(), before);
    assert_eq!(heap.allocation_debt(), 0);
    assert!(member::delete(&mut heap, object, key).unwrap());
    heap.collect([object, key]);
    assert_eq!(heap.counts().symbols, 0);
    member::set(&mut heap, object, key, Value::Int(42)).unwrap();
    assert_eq!(
        member::get(&mut heap, object, key).unwrap().as_integer(),
        Some(42)
    );
    assert_eq!(heap.collect([]).after, HeapCounts::default());
}

#[test]
fn named_members_preserve_utf16_and_plain_missing_members_are_errors() {
    let mut heap = Heap::new();
    let object = Value::Obj(heap.alloc_object().into());
    let key = Value::Str(heap.alloc_string(vec![0xd800, 0, 65]));
    let same = Value::Str(heap.alloc_string(vec![0xd800]));
    assert!(matches!(
        member::get(&mut heap, object, key),
        Err(member::MemberError::Missing)
    ));
    member::set(&mut heap, object, key, Value::Int(9)).unwrap();
    assert_eq!(
        member::get(&mut heap, object, same).unwrap().as_integer(),
        Some(9)
    );
    assert!(member::delete(&mut heap, object, same).unwrap());
}

#[test]
fn native_text_and_converted_keys_do_not_leave_temporary_gc_strings() {
    let mut heap = Heap::new();
    let object = Value::Obj(heap.alloc_dictionary().into());
    let object_key = Value::Obj(heap.alloc_object().into());
    let keys = [Value::Real(1.5), Value::Int(7), object_key];
    for key in keys {
        member::set(&mut heap, object, key, Value::Int(1)).unwrap();
    }
    heap.collect([object, object_key]);
    let counts = heap.counts();
    for key in keys {
        for n in 0..100 {
            member::set(&mut heap, object, key, Value::Int(n)).unwrap();
            assert_eq!(
                member::get(&mut heap, object, key).unwrap().as_integer(),
                Some(n)
            );
            assert!(!tjs_core::string::units(&heap, key).unwrap().is_empty());
        }
    }
    assert_eq!(heap.counts(), counts);
    assert_eq!(heap.allocation_debt(), 0);
    assert_eq!(heap.collect([]).after, HeapCounts::default());
}

#[test]
fn verifier_checks_every_member_operand_including_the_store_value() {
    assert_eq!(size_of::<Instruction>(), 16);
    let r = Register;
    for tail in [
        Instruction::CharacterCode {
            dst: r(0),
            src: r(2),
        },
        Instruction::CharacterFrom {
            dst: r(0),
            src: r(2),
        },
        Instruction::CharacterCode {
            dst: r(3),
            src: r(0),
        },
        Instruction::CharacterFrom {
            dst: r(3),
            src: r(0),
        },
        Instruction::LogicalAnd {
            dst: r(0),
            lhs: r(0),
            rhs: r(2),
        },
        Instruction::LogicalOr {
            dst: r(0),
            lhs: r(2),
            rhs: r(0),
        },
        Instruction::IsValid {
            dst: r(0),
            src: r(2),
        },
        Instruction::Invalidate {
            dst: r(0),
            src: r(2),
        },
        Instruction::TypeOf {
            dst: r(0),
            src: r(2),
        },
        Instruction::InstanceOf {
            dst: r(0),
            lhs: r(0),
            rhs: r(2),
        },
        Instruction::TypeOfMember {
            dst: r(0),
            object: r(0),
            key: r(2),
            computed: true,
        },
        Instruction::GetRawName {
            dst: r(0),
            key: r(2),
        },
        Instruction::SetRawName {
            key: r(1),
            value: r(2),
        },
        Instruction::GetRawMember {
            dst: r(0),
            object: r(2),
            key: r(1),
        },
        Instruction::SetRawMember {
            object: r(0),
            key: r(1),
            value: r(2),
        },
        Instruction::GetProperty {
            dst: r(0),
            src: r(2),
        },
        Instruction::SetProperty {
            property: r(2),
            value: r(0),
        },
        Instruction::AddClassInfo,
        Instruction::SetMember {
            object: r(0),
            key: r(1),
            value: r(2),
        },
        Instruction::SetMember {
            object: r(0),
            key: r(1),
            value: r(3),
        },
        Instruction::GetMember {
            dst: r(0),
            object: r(2),
            key: r(1),
        },
        Instruction::DeleteMember {
            dst: r(0),
            object: r(0),
            key: r(2),
        },
        Instruction::GetMember {
            dst: r(3),
            object: r(0),
            key: r(1),
        },
    ] {
        let code = vec![
            Instruction::NewDictionary { dst: r(0) },
            Instruction::LoadInt {
                dst: r(1),
                value: 0,
            },
            tail,
            Instruction::Return { src: r(0) },
        ];
        assert!(Module::new(3, code, vec![None; 4]).is_err());
    }
}
