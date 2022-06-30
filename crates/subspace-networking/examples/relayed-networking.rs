use env_logger::Env;
use futures::channel::mpsc;
use futures::StreamExt;
use libp2p::gossipsub::Sha256Topic;
// use libp2p::multiaddr::Protocol; //TODO
use std::sync::Arc;
use std::time::Duration;
use libp2p::Multiaddr;
use libp2p::swarm::AddressScore;
use subspace_networking::{Config, RelayConfiguration};

const TOPIC: &str = "Foo";

#[tokio::main]
async fn main() {
    env_logger::init_from_env(Env::new().default_filter_or("info"));

    // NODE 1 - Relay
    let node_1_addr: Multiaddr = "/ip4/192.168.1.215/tcp/50000".parse().unwrap();
    let node_1_addr2: Multiaddr = "/memory/50000".parse().unwrap();
    let config_1 = Config {
        listen_on: vec![
            node_1_addr.clone(),
            node_1_addr2.clone(),
        ],
        allow_non_globals_in_dht: true,
        relay_config: RelayConfiguration::Server(node_1_addr2.clone()),
        ..Config::with_generated_keypair()
    };

    let (node_1, mut node_runner_1) = subspace_networking::create(config_1).await.unwrap();
//    node_runner_1.swarm().add_external_address(node_1_addr, AddressScore::Infinite);
//    node_runner_1.swarm().add_external_address(node_1_addr2.clone(), AddressScore::Infinite);
    //relay.add_external_address(relay_addr.clone(), AddressScore::Infinite);

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

    let _node_1_address = node_1_addresses_receiver.next().await.unwrap();

    // NODE 2 - Server

    let c = Config::with_generated_keypair();

    let config_2 = Config {
        // bootstrap_nodes: vec![node_1_address
        //     .clone()
        //     .with(Protocol::P2p(node_1.id().into()))],
        listen_on: vec![
            // "/ip4/192.168.1.215/tcp/0".parse().unwrap(),
            // format!("/ip4/192.168.1.215/tcp/50000/p2p/{}/p2p-circuit", node_1.id(), )
            //     .parse()
            //     .unwrap(),
            format!("/memory/50000/p2p/{}/p2p-circuit", node_1.id(), )
                .parse()
                .unwrap(),
        ],
        // listen_on: vec![
        //     "/ip4/192.168.1.215/tcp/0".parse().unwrap(),
        //     format!("/ip4/192.168.1.215/tcp/50000/p2p/{}/p2p-circuit/p2p/{}", node_1.id(),c.keypair.public().to_peer_id() )
        //         .parse()
        //         .unwrap(),
        // ],
        allow_non_globals_in_dht: true,
        relay_config: RelayConfiguration::Client(node_1_addr2.clone()), //TODO
        ..c
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

    let mut subscription = node_2.subscribe(Sha256Topic::new(TOPIC)).await.unwrap();

    // NODE 3 - requester

    let config_3 = Config {
        bootstrap_nodes: vec![
            // node_1_address.clone()
            //     .with(Protocol::P2p(node_1.id().into())),


            //
            // format!(
            //     "/ip4/192.168.1.215/tcp/50000/p2p/{}/p2p-circuit/p2p/{}",
            //     node_1.id(),
            //     node_2.id()
            // )
            //     .try_into()
            //     .unwrap(),
            //
            format!(
                "/memory/50000/p2p/{}/p2p-circuit/p2p/{}",
                node_1.id(),
                node_2.id()
            )
                .try_into()
                .unwrap(),
        ],
        listen_on: vec![
        //    "/ip4/192.168.1.215/tcp/0".parse().unwrap(),
            // format!("/ip4/192.168.1.215/tcp/50000/p2p/{}/p2p-circuit", node_1.id(),)
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
            .publish(Sha256Topic::new(TOPIC), "hello".to_string().into_bytes())
            .await
            .unwrap();
    });

    let message = subscription.next().await.unwrap();
    println!("Got message: {}", String::from_utf8_lossy(&message));

    tokio::time::sleep(Duration::from_secs(3)).await;
}

