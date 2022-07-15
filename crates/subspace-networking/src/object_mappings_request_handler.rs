// Copyright (C) 2020-2022 Parity Technologies (UK) Ltd.
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

//! Helper for incoming object mappings requests.
//!
//! Handle (i.e. answer) incoming object mappings requests from a remote peer received via
//! `crate::request_responses::RequestResponsesBehaviour` with
//! [`ObjectMappingsRequestHandler`](ObjectMappingsRequestHandler).

use crate::request_responses::{
    IncomingRequest, OutgoingResponse, ProtocolConfig, RequestResponseHandlerRunner,
};
use async_trait::async_trait;
use futures::channel::mpsc;
use futures::prelude::*;
use libp2p::PeerId;
use parity_scale_codec::{Decode, Encode};
use std::sync::Arc;
use subspace_core_primitives::objects::GlobalObject;
use subspace_core_primitives::Sha256Hash;
use tracing::{debug, trace};

const LOG_TARGET: &str = "object-mappings-request-response-handler";
// Could be changed after the production feedback.
const REQUESTS_BUFFER_SIZE: usize = 50;
/// Pieces-by-range-protocol name.
pub const PROTOCOL_NAME: &str = "/subspace/object-mappings/0.1.0";

/// Object-mapping protocol request.
#[derive(Debug, Clone, Eq, PartialEq, Encode, Decode)]
pub struct ObjectMappingsRequest {
    /// Object hash (32-bytes)
    pub object_hash: Sha256Hash,
}

/// Object-mapping protocol request.
#[derive(Debug, Default, PartialEq, Eq, Clone, Encode, Decode)]
pub struct ObjectMappingsResponse {
    /// Returned data.
    pub object_mapping: Option<GlobalObject>,
}

/// Type alias for the actual external request handler.
pub type ExternalObjectMappingsRequestHandler =
    Arc<dyn (Fn(&ObjectMappingsRequest) -> Option<ObjectMappingsResponse>) + Send + Sync + 'static>;

// Contains pieces-by-range request handler structure
pub(crate) struct ObjectMappingsRequestHandler {
    request_receiver: mpsc::Receiver<IncomingRequest>,
    request_handler: ExternalObjectMappingsRequestHandler,
}

impl ObjectMappingsRequestHandler {
    /// Create a new [`ObjectMappingsRequestHandler`].
    pub fn new(request_handler: ExternalObjectMappingsRequestHandler) -> (Self, ProtocolConfig) {
        let (request_sender, request_receiver) = mpsc::channel(REQUESTS_BUFFER_SIZE);

        let mut protocol_config = ProtocolConfig::new(PROTOCOL_NAME.into());
        protocol_config.inbound_queue = Some(request_sender);

        (
            Self {
                request_receiver,
                request_handler,
            },
            protocol_config,
        )
    }

    // Invokes external object mappings protocol handler.
    fn handle_request(
        &mut self,
        peer: PeerId,
        payload: Vec<u8>,
    ) -> Result<Vec<u8>, ObjectMappingsRequestHandlerRequestError> {
        trace!(%peer, "Handling request...");
        let request = ObjectMappingsRequest::decode(&mut payload.as_slice())
            .map_err(|_| ObjectMappingsRequestHandlerRequestError::InvalidRequestFormat)?;
        let response = (self.request_handler)(&request);

        // Return the result with treating None as an empty(default) response.
        Ok(response.unwrap_or_default().encode())
    }
}

#[async_trait]
impl RequestResponseHandlerRunner for ObjectMappingsRequestHandler {
    /// Run [`ObjectMappingsRequestHandler`].
    async fn run(&mut self) {
        while let Some(request) = self.request_receiver.next().await {
            let IncomingRequest {
                peer,
                payload,
                pending_response,
            } = request;

            match self.handle_request(peer, payload) {
                Ok(response_data) => {
                    let response = OutgoingResponse {
                        result: Ok(response_data),
                        sent_feedback: None,
                    };

                    match pending_response.send(response) {
                        Ok(()) => trace!(target: LOG_TARGET, %peer, "Handled request",),
                        Err(_) => debug!(
                            target: LOG_TARGET,
                            %peer,
                            "Failed to handle request: {}",
                            ObjectMappingsRequestHandlerRequestError::SendResponse
                        ),
                    };
                }
                Err(e) => {
                    debug!(target: LOG_TARGET, %e, "Failed to handle request.",);

                    let response = OutgoingResponse {
                        result: Err(()),
                        sent_feedback: None,
                    };

                    if pending_response.send(response).is_err() {
                        debug!(
                            target: LOG_TARGET,
                            %peer,
                            "Failed to handle request: {}", ObjectMappingsRequestHandlerRequestError::SendResponse
                        );
                    };
                }
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum ObjectMappingsRequestHandlerRequestError {
    #[error("Failed to send response.")]
    SendResponse,

    #[error("Incorrect request format.")]
    InvalidRequestFormat,
}
