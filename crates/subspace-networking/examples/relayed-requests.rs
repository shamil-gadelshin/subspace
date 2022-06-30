use env_logger::Env;
use futures::channel::mpsc;
use futures::StreamExt;
//use libp2p::multiaddr::Protocol; // TODO
use std::sync::Arc;
use std::time::Duration;
use subspace_core_primitives::{FlatPieces, Piece, PieceIndexHash};
use subspace_networking::{Config, PiecesByRangeRequest, PiecesByRangeResponse, PiecesToPlot, RelayConfiguration};
use libp2p::Multiaddr;
use libp2p::swarm::AddressScore;

#[tokio::main]
async fn main() {
    env_logger::init_from_env(Env::new().default_filter_or("info"));

    // NODE 1 - Relay
    let node_1_addr: Multiaddr = "/ip4/127.0.0.1/tcp/50000".parse().unwrap();
    let node_1_addr2: Multiaddr = "/memory/10".parse().unwrap();
    let config_1 = Config {
        listen_on: vec![
            node_1_addr.clone(),
            node_1_addr2.clone()],
        allow_non_globals_in_dht: true,
        relay_config: RelayConfiguration::Server(node_1_addr2.clone()), //TODO
        ..Config::with_generated_keypair()
    };

    let (node_1, mut node_runner_1) = subspace_networking::create(config_1).await.unwrap();

//    node_runner_1.swarm().add_external_address(node_1_addr, AddressScore::Infinite);
//    node_runner_1.swarm().add_external_address(node_1_addr2.clone(), AddressScore::Infinite);

    println!("Node 1 (relay) ID is {}", node_1.id());

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

    let _node_1_address = node_1_addresses_receiver.next().await.unwrap(); //TODO

    // NODE 2 - Server

    let config_2 = Config {
        // bootstrap_nodes: vec![node_1_address
        //     .clone()
        //     .with(Protocol::P2p(node_1.id().into()))],
        listen_on: vec![
            //            "/ip4/0.0.0.0/tcp/0".parse().unwrap(),
            // format!("/ip4/127.0.0.1/tcp/50000/p2p/{}/p2p-circuit", node_1.id(),)
            //     .parse()
            //     .unwrap(),
            format!("/memory/10/p2p/{}/p2p-circuit", node_1.id(),)
                .parse()
                .unwrap(),
        ],
        allow_non_globals_in_dht: true,
        pieces_by_range_request_handler: Arc::new(|req| {
            println!("Request handler for request: {:?}", req);

            let piece_bytes: Vec<u8> = Piece::default().into();
            let flat_pieces = FlatPieces::try_from(piece_bytes).unwrap();
            let pieces = PiecesToPlot {
                piece_indexes: vec![1],
                pieces: flat_pieces,
            };

            Some(PiecesByRangeResponse {
                pieces,
                next_piece_index_hash: None,
            })
        }),
        relay_config: RelayConfiguration::Client(node_1_addr2.clone()), //TODO
        ..Config::with_generated_keypair()
    };
    let (node_2, node_runner_2) = subspace_networking::create(config_2).await.unwrap();

    println!("Node 2 (server) ID is {}", node_2.id());

    // let (node_2_addresses_sender, mut node_2_addresses_receiver) = mpsc::unbounded();
    // node_1
    //     .on_new_listener(Arc::new(move |address| {
    //         node_1_addresses_sender
    //             .unbounded_send(address.clone())
    //             .unwrap();
    //     }))
    //     .detach();

    tokio::spawn(async move {
        node_runner_2.run().await;
    });

    // NODE 3 - requester

    let config_3 = Config {
        bootstrap_nodes: vec![
            // node_1_address.clone()
            //     .with(Protocol::P2p(node_1.id().into())),

            format!(
              "/ip4/127.0.0.1/tcp/50000/p2p/{}/p2p-circuit/p2p/{}",
              node_1.id(),
              node_2.id()
          )
          .try_into()
          .unwrap(),
          //   format!(
          //     "/memory/10/p2p/{}/p2p-circuit/p2p/{}",
          //     node_1.id(),
          //     node_2.id()
          // )
          // .try_into()
          // .unwrap(),
        ],
        listen_on: vec![
 //                     "/ip4/0.0.0.0/tcp/0".parse().unwrap(),
         //             "/memory/200".parse().unwrap(),
            // format!("/ip4/127.0.0.1/tcp/50000/p2p/{}/p2p-circuit", node_1.id(),)
            //     .parse()
            //     .unwrap(),
        ],
        allow_non_globals_in_dht: true,
        relay_config: RelayConfiguration::Client(node_1_addr2.clone()), //TODO
        ..Config::with_generated_keypair()
    };

    let (node_3, node_runner_3) = subspace_networking::create(config_3).await.unwrap();

    println!("Node 3 (requester) ID is {}", node_3.id());

    tokio::spawn(async move {
        node_runner_3.run().await;
    });

    tokio::time::sleep(Duration::from_secs(1)).await;

    tokio::spawn(async move {
        node_3
            .send_pieces_by_range_request(
                node_2.id(),
                PiecesByRangeRequest {
                    from: PieceIndexHash([1u8; 32]),
                    to: PieceIndexHash([1u8; 32]),
                },
            )
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_secs(5)).await;
}
