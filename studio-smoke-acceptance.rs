#[path = "studio-smoke-normalize.rs"]
mod normalize;

#[test]
fn converts_the_entire_input_range() {
    assert_eq!(normalize::milliseconds(0), 0);
    assert_eq!(normalize::milliseconds(1), 1_000);
    assert_eq!(normalize::milliseconds(u32::MAX), 4_294_967_295_000);
}
