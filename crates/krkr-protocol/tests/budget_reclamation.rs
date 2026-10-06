use krkr_protocol::budget::Budget;

#[test]
fn reclamation_estimates_shared_ancestors_without_releasing_live_permits() {
    let root = Budget::new(1000);
    let graphics = root.child(800);
    let resident = graphics.child(600);
    let scratch = graphics.child(500);
    let outside = Budget::new(2000);
    let live = resident.reserve(200).unwrap();
    let image = resident.reserve(350).unwrap();
    let work = scratch.reserve(200).unwrap();
    let other = root.reserve(200).unwrap();
    let unrelated = outside.reserve(1500).unwrap();
    assert_eq!(scratch.available(), 50);
    let retired = [&image, &unrelated];
    assert_eq!(scratch.available_after_releasing(retired.into_iter()), 300);
    assert_eq!(resident.available_after_releasing(retired.into_iter()), 400);
    let retired = [&image, &work, &other, &unrelated];
    assert_eq!(scratch.available_after_releasing(retired.into_iter()), 500);
    assert_eq!(resident.available_after_releasing(retired.into_iter()), 400);
    assert_eq!(root.used(), 950);
    assert!(
        scratch.reserve(51).is_err(),
        "estimate released real capacity"
    );
    drop((image, work, other, unrelated));
    assert_eq!(scratch.available(), 500);
    assert_eq!(resident.available(), 400);
    drop(live);
}
