#[test]
fn probe() {
    for r in filmcraft_engine::coach_ask::retrieve("I use the same lower-third logo treatment in 15 places, if I change it once will all of them update?", 8) {
        eprintln!("PROBE {:?} {:?}", r.header.map(|h| h.id.clone()), r.score);
    }
}
