use env_logger::Env;
use futures::channel::mpsc;
use futures::StreamExt;
use libp2p::multiaddr::Protocol;
use std::sync::Arc;
use std::time::Duration;
use subspace_core_primitives::{crypto, Piece, PieceIndexHash};
use subspace_networking::{Config, PiecesByRangeResponse};

#[tokio::main]
async fn main() {
    env_logger::init_from_env(Env::new().default_filter_or("info"));

    let config_1 = Config {
        listen_on: vec!["/ip4/0.0.0.0/tcp/0".parse().unwrap()],
        value_getter: Arc::new(|key| {
            // Return the reversed digest as a value
            Some(key.digest().iter().copied().rev().collect())
        }),
        request_handler: Arc::new(|req| {
            println!("Request handler for request: {:?}", req);
            let response = Some(PiecesByRangeResponse {
                pieces: vec![Piece::default()],
                next_piece_hash_index: Some(PieceIndexHash([0; 32])),
            });

            println!("Sending response... ");

            std::thread::sleep(Duration::from_secs(1));
            response
        }),
        allow_non_globals_in_dht: true,
        ..Config::with_generated_keypair()
    };
    let (node_1, mut node_runner_1) = subspace_networking::create(config_1).await.unwrap();

    println!("Node 1 ID is {}", node_1.id());

    let (node_1_addresses_sender, mut node_1_addresses_receiver) = mpsc::unbounded();
    node_1
        .on_new_listener(Arc::new(move |address| {
            node_1_addresses_sender
                .unbounded_send(address.clone())
                .unwrap();
        }))
        .detach();

    tokio::spawn(async move {
        node_runner_1.run().await;
    });

    let config_2 = Config {
        bootstrap_nodes: vec![node_1_addresses_receiver
            .next()
            .await
            .unwrap()
            .with(Protocol::P2p(node_1.id().into()))],
        listen_on: vec!["/ip4/0.0.0.0/tcp/0".parse().unwrap()],
        allow_non_globals_in_dht: true,
        ..Config::with_generated_keypair()
    };

    let (node_2, mut node_runner_2) = subspace_networking::create(config_2).await.unwrap();

    println!("Node 2 ID is {}", node_2.id());

    tokio::spawn(async move {
        node_runner_2.run().await;
    });

    tokio::time::sleep(Duration::from_secs(1)).await;

    let hashed_peer_id = PieceIndexHash(crypto::sha256_hash(node_1.id().to_bytes()));

    let stream_future = node_2.get_pieces_by_range(hashed_peer_id, hashed_peer_id);
    if let Ok(mut stream) = stream_future.await {
        while let Some(value) = stream.next().await {
            println!("Piece found: {:?}", value);
        }
    } else {
        println!("Stream error");
    }

    tokio::time::sleep(Duration::from_secs(5)).await;
}
