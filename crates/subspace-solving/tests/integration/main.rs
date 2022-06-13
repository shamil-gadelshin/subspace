use subspace_core_primitives::U256;

mod codec;

#[test]
fn piece_distance_middle() {
    assert_eq!(U256::MIDDLE, U256::MAX / 2);
}
