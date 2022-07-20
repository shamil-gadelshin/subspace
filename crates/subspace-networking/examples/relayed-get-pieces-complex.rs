use futures::channel::oneshot;
use futures::StreamExt;
use libp2p::multiaddr::Protocol;
use libp2p::{identity, PeerId};
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;
use subspace_core_primitives::{FlatPieces, Piece, PieceIndexHash};
use subspace_networking::{Config, PiecesByRangeResponse, PiecesToPlot, RpcProtocol};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    // Relay node
    let config_1 = Config {
        listen_on: vec!["/ip4/127.0.0.1/tcp/0".parse().unwrap()],
        allow_non_globals_in_dht: true,
        ..Config::with_generated_keypair()
    };

    let (relay_node, mut relay_node_runner) = subspace_networking::create(config_1).await.unwrap();

    println!("Relay Node ID is {}", relay_node.id());

    let (relay_node_address_sender, relay_node_address_receiver) = oneshot::channel();
    let on_new_listener_handler = relay_node.on_new_listener(Arc::new({
        let relay_node_address_sender = Mutex::new(Some(relay_node_address_sender));

        move |address| {
            if matches!(address.iter().next(), Some(Protocol::Ip4(_))) {
                if let Some(relay_node_address_sender) = relay_node_address_sender.lock().take() {
                    relay_node_address_sender.send(address.clone()).unwrap();
                }
            }
        }
    }));

    tokio::spawn(async move {
        relay_node_runner.run().await;
    });

    // Wait for relay to know its address
    let relay_node_addr = relay_node_address_receiver.await.unwrap();
    drop(on_new_listener_handler);

    let mut bootstrap_nodes = Vec::new();
    let mut expected_node_id = PeerId::random();

    const TOTAL_NODE_COUNT: usize = 100;
    const EXPECTED_NODE_INDEX: usize = 75;

    let expected_response = {
        let piece_bytes: Vec<u8> = Piece::default().into();
        let flat_pieces = FlatPieces::try_from(piece_bytes).unwrap();
        let pieces = PiecesToPlot {
            piece_indexes: vec![1],
            pieces: flat_pieces,
        };

        PiecesByRangeResponse {
            pieces,
            next_piece_index_hash: None,
        }
    };

    let mut nodes = Vec::with_capacity(TOTAL_NODE_COUNT);
    for i in 0..TOTAL_NODE_COUNT {
        let local_response = expected_response.clone();
        let config = Config {
            bootstrap_nodes: bootstrap_nodes.clone(),
            allow_non_globals_in_dht: true,
            request_response_protocols: vec![RpcProtocol::PiecesByRange(Some(Arc::new(
                move |_| {
                    if i != EXPECTED_NODE_INDEX {
                        return None;
                    }

                    println!("Sending response from Node Index {}... ", i);

                    std::thread::sleep(Duration::from_secs(1));
                    Some(local_response.clone())
                },
            )))],
            ..Config::with_generated_keypair()
        };
        let (node, mut node_runner) = relay_node.spawn(config).await.unwrap();

        println!("Node {} ID is {}", i, node.id());

        tokio::spawn(async move {
            node_runner.run().await;
        });

        tokio::time::sleep(Duration::from_millis(40)).await;

        let address = relay_node_addr
            .clone()
            .with(Protocol::P2p(relay_node.id().into()))
            .with(Protocol::P2pCircuit)
            .with(Protocol::P2p(node.id().into()));

        bootstrap_nodes.push(address);

        if i == EXPECTED_NODE_INDEX {
            expected_node_id = node.id();
        }

        nodes.push(node);
    }

    // Debug:
    // println!("Bootstrap NODES: {:?}", bootstrap_nodes);

    let config = Config {
        bootstrap_nodes,
        listen_on: vec!["/ip4/0.0.0.0/tcp/0".parse().unwrap()],
        allow_non_globals_in_dht: true,
        request_response_protocols: vec![RpcProtocol::PiecesByRange(None)],
        ..Config::with_generated_keypair()
    };

    let (node, mut node_runner) = subspace_networking::create(config).await.unwrap();

    println!("Source Node ID is {}", node.id());
    println!("Expected Peer ID:{}", expected_node_id);

    tokio::spawn(async move {
        node_runner.run().await;
    });

    tokio::time::sleep(Duration::from_secs(2)).await;

    let encoding = expected_node_id.as_ref().digest();
    let public_key = identity::PublicKey::from_protobuf_encoding(encoding)
        .expect("Invalid public key from PeerId.");
    let peer_id_public_key = if let identity::PublicKey::Sr25519(pk) = public_key {
        pk.encode()
    } else {
        panic!("Expected PublicKey::Sr25519")
    };

    // create a range from expected peer's public key
    let from = {
        let mut buf = peer_id_public_key;
        buf[16] = 0;
        PieceIndexHash::from(buf)
    };
    let to = {
        let mut buf = peer_id_public_key;
        buf[16] = 50;
        PieceIndexHash::from(buf)
    };

    let stream_future = node.get_pieces_by_range(from, to);
    let mut stream = stream_future.await.unwrap();
    if let Some(value) = stream.next().await {
        if value != expected_response.pieces {
            panic!("UNEXPECTED RESPONSE")
        }

        println!("Received expected response.");
    }

    tokio::time::sleep(Duration::from_secs(1)).await;
    println!("Exiting..");
}
