//! The intensity of a running node changes live (the settings
//! registry's crawler intensity, §11).

use std::net::SocketAddr;
use std::time::Duration;

use lodestar_dht::node::{Config, Intensity, Node};

#[tokio::test]
async fn a_running_node_takes_a_new_intensity() {
    let config = Config {
        bind: vec![SocketAddr::from(([127, 0, 0, 1], 0))],
        intensity: Intensity::Aggressive,
        bootstrap: Vec::new(),
        state_path: None,
        tick: Duration::from_millis(50),
        ..Config::default()
    };
    let node = Node::start(config, None).await.unwrap();
    assert_eq!(
        node.snapshot().await.unwrap().intensity,
        Intensity::Aggressive
    );
    let handle = node.handle();
    assert!(handle.set_intensity(Intensity::Low).await);
    // Commands are handled in order, so the snapshot sees the change.
    assert_eq!(node.snapshot().await.unwrap().intensity, Intensity::Low);
    node.shutdown().await.unwrap();
    assert!(!handle.set_intensity(Intensity::Balanced).await);
}
