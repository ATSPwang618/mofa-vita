use super::*;

#[test]
fn growing_prefixes_during_sweep_keep_the_tail_and_later_shrink_it() {
    let mut heap = Heap::new();
    let short = heap.alloc_string(vec![65]);
    let suffix = heap.alloc_string(vec![66; 100_000]);
    let long = heap.append_strings(short, suffix).unwrap();
    for _ in 0..20 {
        heap.alloc_object();
    }
    let root = heap.root(Value::Str(long));
    while heap.collect_step([], 1).phase != CollectionPhase::Sweep {}
    let extra = heap.alloc_string(vec![67, 68]);
    let longer = heap.append_strings(long, extra).unwrap();
    heap.set_root(root, Value::Str(longer)).unwrap();
    while heap.collect_step([], 1).completed.is_none() {}
    assert_eq!(heap.string(longer).unwrap().len(), 100_003);
    assert_eq!(&heap.string(longer).unwrap()[100_001..], &[67, 68]);
    assert!(heap.string(short).is_err());
    // A separate branch retains a genuinely short prefix of a large buffer.
    let short = heap.alloc_string(vec![70]);
    let large = heap.alloc_string(vec![71; 100_000]);
    heap.append_strings(short, large).unwrap();
    heap.set_root(root, Value::Str(short)).unwrap();
    while heap.collect_step([], 1).completed.is_none() {}
    assert_eq!(heap.string(short).unwrap(), &[70]);
    let strings::Data::Prefix { buffer, .. } = heap.strings[short.0].data else {
        panic!()
    };
    assert!(heap.string_buffers[buffer].data.units.capacity() < 1024);
}
