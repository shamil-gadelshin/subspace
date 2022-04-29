use crate::{
    plotting, Archiving, Commitments, Farming, Identity, NodeRpcClient, ObjectMappings, Plot,
    PlotError, RpcClient,
};
use anyhow::anyhow;
use futures::stream::{FuturesUnordered, StreamExt};
use log::info;
use rayon::prelude::*;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use subspace_core_primitives::{PublicKey, PIECE_SIZE};
use subspace_networking::{
    libp2p::{identity::ed25519, multiaddr::Protocol, Multiaddr},
    multimess::MultihashCode,
    Config,
};
use subspace_solving::SubspaceCodec;

/// Abstraction around having multiple `Plot`s, `Farming`s and `Plotting`s.
///
/// It is needed because of the limit of a single plot size from the consensus
/// (`pallet_subspace::MaxPlotSize`) in order to support any amount of disk space from user.
// TODO: tie `plots`, `farmings`, `networking_nodes`, `networking_node_runners` together as they
// always will have same length.
pub struct MultiFarming {
    pub plots: Vec<Plot>,
    farmings: Vec<Farming>,
    networking_node_runners: Vec<subspace_networking::NodeRunner>,
    archiving: Archiving,
}

fn get_plot_sizes(total_plot_size: u64, max_plot_size: u64) -> Vec<u64> {
    // TODO: we need to remember plot size in order to prune unused plots in future if plot size is
    // less than it was specified before.
    // TODO: Piece count should account for database overhead of various additional databases
    // For now assume 80% will go for plot itself
    let total_plot_size = total_plot_size * 4 / 5 / PIECE_SIZE as u64;

    let plot_sizes =
        std::iter::repeat(max_plot_size).take((total_plot_size / max_plot_size) as usize);
    if total_plot_size % max_plot_size > 0 {
        plot_sizes
            .chain(std::iter::once(total_plot_size % max_plot_size))
            .collect::<Vec<_>>()
    } else {
        plot_sizes.collect()
    }
}

impl MultiFarming {
    /// Starts multiple farmers with any plot sizes which user gives
    pub async fn new(
        base_directory: PathBuf,
        client: NodeRpcClient,
        object_mappings: ObjectMappings,
        reward_address: PublicKey,
        best_block_number_check_interval: Duration,
        total_plot_size: u64,
        max_plot_size: u64,
        bootstrap_nodes: Vec<Multiaddr>,
        listen_on: Vec<Multiaddr>,
    ) -> anyhow::Result<Self> {
        let plot_sizes = get_plot_sizes(total_plot_size, max_plot_size);
        Self::new_inner(
            base_directory.clone(),
            client,
            object_mappings,
            reward_address,
            best_block_number_check_interval,
            plot_sizes,
            move |plot_index, address, max_piece_count| {
                Plot::open_or_create(
                    base_directory.join(format!("plot{plot_index}")),
                    address,
                    max_piece_count,
                )
            },
            bootstrap_nodes,
            listen_on,
        )
        .await
    }

    async fn new_inner(
        base_directory: impl AsRef<Path>,
        client: NodeRpcClient,
        object_mappings: ObjectMappings,
        reward_address: PublicKey,
        best_block_number_check_interval: Duration,
        plot_sizes: Vec<u64>,
        new_plot: impl Fn(usize, PublicKey, u64) -> Result<Plot, PlotError> + Clone + Send + 'static,
        bootstrap_nodes: Vec<Multiaddr>,
        listen_on: Vec<Multiaddr>,
    ) -> anyhow::Result<Self> {
        let mut plots = Vec::with_capacity(plot_sizes.len());
        let mut subspace_codecs = Vec::with_capacity(plot_sizes.len());
        let mut commitments = Vec::with_capacity(plot_sizes.len());
        let mut farmings = Vec::with_capacity(plot_sizes.len());
        let mut networking_node_runners = Vec::with_capacity(plot_sizes.len());

        let mut results = plot_sizes
            .into_iter()
            .enumerate()
            .map(|(plot_index, max_plot_pieces)| {
                let base_directory = base_directory.as_ref().to_owned();
                let client = client.clone();
                let new_plot = new_plot.clone();

                tokio::task::spawn_blocking(move || {
                    let base_directory = base_directory.join(format!("plot{plot_index}"));
                    std::fs::create_dir_all(&base_directory)?;

                    let identity = Identity::open_or_create(&base_directory)?;
                    let public_key = identity.public_key().to_bytes().into();

                    // TODO: This doesn't account for the fact that node can
                    // have a completely different history to what farmer expects
                    info!("Opening plot");
                    let plot = new_plot(plot_index, public_key, max_plot_pieces)?;

                    info!("Opening commitments");
                    let plot_commitments = Commitments::new(base_directory.join("commitments"))?;

                    let subspace_codec = SubspaceCodec::new(identity.public_key());

                    // Start the farming task
                    let farming = Farming::start(
                        plot.clone(),
                        plot_commitments.clone(),
                        client.clone(),
                        identity.clone(),
                        reward_address,
                    );

                    Ok::<_, anyhow::Error>((
                        identity,
                        plot,
                        subspace_codec,
                        plot_commitments,
                        farming,
                    ))
                })
            })
            .collect::<Vec<_>>()
            .into_iter();

        // Only the first node listens on publicly known addresses
        let first_node_listeners = {
            let (identity, plot, subspace_codec, plot_commitments, farming) = results
                .next()
                .expect("There is always at least one plot")
                .await
                .unwrap()?;

            let (node, node_runner) = subspace_networking::create(Config {
                bootstrap_nodes: bootstrap_nodes.clone(),
                value_getter: Arc::new({
                    let plot = plot.clone();
                    move |key| {
                        let code = key.code();

                        if code != u64::from(MultihashCode::Piece)
                            && code != u64::from(MultihashCode::PieceIndex)
                        {
                            return None;
                        }

                        let piece_index = u64::from_le_bytes(
                            key.digest()[..std::mem::size_of::<u64>()].try_into().ok()?,
                        );
                        plot.read(piece_index)
                            .ok()
                            .and_then(|mut piece| {
                                subspace_codec
                                    .decode(&mut piece, piece_index)
                                    .ok()
                                    .map(move |()| piece)
                            })
                            .map(|piece| piece.to_vec())
                    }
                }),
                allow_non_globals_in_dht: true,
                listen_on,
                ..Config::with_keypair(ed25519::Keypair::from(
                    // TODO: substitute with `sr25519` once its support is done in libp2p
                    ed25519::SecretKey::from_bytes(identity.secret_key().to_bytes())
                        .expect("Always valid"),
                ))
            })
            .await?;

            node.on_new_listener(Arc::new({
                let node_id = node.id();

                move |multiaddr| {
                    info!(
                        "Listening on {}",
                        multiaddr.clone().with(Protocol::P2p(node_id.into()))
                    );
                }
            }))
            .detach();

            let listeners = node.listeners();

            networking_node_runners.push(node_runner);
            plots.push(plot);
            subspace_codecs.push(subspace_codec);
            commitments.push(plot_commitments);
            farmings.push(farming);

            dbg!(listeners)
        };

        let mut bootstrap_nodes = first_node_listeners;

        for result_future in results {
            let (identity, plot, subspace_codec, plot_commitments, farming) =
                result_future.await.unwrap()?;

            let (node, node_runner) = subspace_networking::create(Config {
                bootstrap_nodes: bootstrap_nodes.clone(),
                value_getter: Arc::new({
                    let plot = plot.clone();
                    move |key| {
                        let code = key.code();

                        if code != u64::from(MultihashCode::Piece)
                            && code != u64::from(MultihashCode::PieceIndex)
                        {
                            return None;
                        }

                        let piece_index = u64::from_le_bytes(
                            key.digest()[..std::mem::size_of::<u64>()].try_into().ok()?,
                        );
                        plot.read(piece_index)
                            .ok()
                            .and_then(|mut piece| {
                                subspace_codec
                                    .decode(&mut piece, piece_index)
                                    .ok()
                                    .map(move |()| piece)
                            })
                            .map(|piece| piece.to_vec())
                    }
                }),
                allow_non_globals_in_dht: true,
                ..Config::with_keypair(ed25519::Keypair::from(
                    // TODO: substitute with `sr25519` once its support is done in libp2p
                    ed25519::SecretKey::from_bytes(identity.secret_key().to_bytes())
                        .expect("Always valid"),
                ))
            })
            .await?;

            node.on_new_listener(Arc::new({
                let node_id = node.id();

                move |multiaddr| {
                    info!(
                        "Listening on {}",
                        multiaddr.clone().with(Protocol::P2p(node_id.into()))
                    );
                }
            }))
            .detach();

            bootstrap_nodes.append(&mut dbg!(node.listeners()));

            networking_node_runners.push(node_runner);
            plots.push(plot);
            subspace_codecs.push(subspace_codec);
            commitments.push(plot_commitments);
            farmings.push(farming);
        }

        let farmer_metadata = client
            .farmer_metadata()
            .await
            .map_err(|error| anyhow!(error))?;

        // Start archiving task
        let archiving = Archiving::start(
            farmer_metadata,
            object_mappings,
            client.clone(),
            best_block_number_check_interval,
            {
                let mut on_pieces_to_plots = plots
                    .iter()
                    .zip(subspace_codecs)
                    .zip(&commitments)
                    .map(|((plot, subspace_codec), commitments)| {
                        plotting::plot_pieces(subspace_codec, plot, commitments.clone())
                    })
                    .collect::<Vec<_>>();

                move |pieces_to_plot| {
                    on_pieces_to_plots
                        .par_iter_mut()
                        .map(|on_pieces_to_plot| {
                            // TODO: It might be desirable to not clone it and instead pick just
                            //  unnecessary pieces and copy pieces once since different plots will
                            //  care about different pieces
                            on_pieces_to_plot(pieces_to_plot.clone())
                        })
                        .reduce(|| true, |result, should_continue| result && should_continue)
                }
            },
        )
        .await?;

        Ok(Self {
            plots,
            farmings,
            archiving,
            networking_node_runners,
        })
    }

    /// Waits for farming and plotting completion (or errors)
    pub async fn wait(self) -> anyhow::Result<()> {
        let mut farming = self
            .farmings
            .into_iter()
            .map(|farming| farming.wait())
            .collect::<FuturesUnordered<_>>();
        let mut node_runners = self
            .networking_node_runners
            .into_iter()
            .map(|mut node_runner| async move { node_runner.run().await })
            .collect::<FuturesUnordered<_>>();

        tokio::select! {
            res = farming.select_next_some() => {
                res?;
            },
            () = node_runners.select_next_some() => {},
            res = self.archiving.wait() => {
                res?;
            },
        }

        Ok(())
    }
}
