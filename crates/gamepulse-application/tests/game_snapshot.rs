#![forbid(unsafe_code)]

use gamepulse_application::{
    GameSnapshot, GameSnapshotStore, SourceProductId, upsert_game_snapshot,
};

#[derive(Default)]
struct RecordingStore {
    source_ids: Vec<u64>,
}

impl GameSnapshotStore for RecordingStore {
    type Error = std::convert::Infallible;

    fn upsert_snapshot(&mut self, snapshot: &GameSnapshot) -> Result<(), Self::Error> {
        self.source_ids.push(snapshot.source_product_id().value());
        Ok(())
    }
}

#[test]
fn application_upsert_delegates_a_validated_snapshot_to_its_port() {
    let snapshot = GameSnapshot::new(
        SourceProductId::new(101).expect("test identity must be valid"),
        "example-game",
        "Example Game",
        "Example description",
        None,
        None,
        Vec::new(),
        Vec::new(),
    )
    .expect("test snapshot must be valid");
    let mut store = RecordingStore::default();

    upsert_game_snapshot(&mut store, &snapshot).expect("recording store must accept snapshot");

    assert_eq!(store.source_ids, [101]);
}
