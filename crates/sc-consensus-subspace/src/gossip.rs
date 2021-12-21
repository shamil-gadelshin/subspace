// Copyright (C) 2021 Subspace Labs, Inc.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

//! # Subspace gossip protocol extension

use prometheus_endpoint::Registry;
use sc_network::PeerId;
use sc_network_gossip::{GossipEngine, ValidationResult, ValidatorContext};
use sp_api::BlockT;
use std::sync::Arc;

const SUBSPACE_GOSSIP_PROTOCOL_NAME: &str = "/subspace/gossip/1";

/// Start Subspace gossip
pub async fn start_subspace_gossip<Block, Network>(
    network: Network,
    prometheus_registry: Option<&Registry>,
) -> !
where
    Block: BlockT,
    Network: sc_network_gossip::Network<Block> + Send + Clone + 'static,
{
    let validator = Validator {
        // TODO
    };

    let mut gossip_engine = GossipEngine::new(
        network,
        SUBSPACE_GOSSIP_PROTOCOL_NAME,
        Arc::new(validator),
        prometheus_registry,
    );

    loop {
        (&mut gossip_engine).await;
    }
}

/// Validation of Subspace-specific gossip
struct Validator {
    // TODO
}

impl<Block: BlockT> sc_network_gossip::Validator<Block> for Validator {
    fn validate(
        &self,
        _context: &mut dyn ValidatorContext<Block>,
        _sender: &PeerId,
        _data: &[u8],
    ) -> ValidationResult<Block::Hash> {
        todo!()
    }
    // TODO
}
