use crate::peer_info::PeerInfo;
use futures::prelude::*;
use parity_scale_codec::{Decode, Encode};
use std::io;

pub const PROTOCOL_NAME: &[u8] = b"/subspace/peer-info/1.0.0";

/// Sends a ping and waits for the pong.
pub async fn send<S>(mut stream: S, pi: PeerInfo) -> io::Result<(S, PeerInfo)>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let send_data = pi.encode();
    let send_len_bytes = (send_data.len() as u32).to_le_bytes();

    stream.write_all(&send_len_bytes).await?;
    stream.write_all(&send_data).await?;
    stream.flush().await?;

    let mut rec_len_bytes = [0u8; 4];
    stream.read_exact(&mut rec_len_bytes).await?;
    let rec_len = u32::from_le_bytes(rec_len_bytes) as usize;
    let mut rec_data = vec![0; rec_len as usize];

    let _received = stream.read_exact(&mut rec_data).await; //TODO

    let received_peer_info = PeerInfo::decode(&mut &*rec_data).unwrap(); //TODO

    Ok((stream, received_peer_info))
}

/// Waits for a ping and sends a pong.
pub async fn recv<S>(mut stream: S, pi: PeerInfo) -> io::Result<(S, PeerInfo)>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut rec_len_bytes = [0u8; 4];
    stream.read_exact(&mut rec_len_bytes).await?;
    let rec_len = u32::from_le_bytes(rec_len_bytes) as usize;
    let mut rec_data = vec![0; rec_len as usize];

    let _received = stream.read_exact(&mut rec_data).await; //TODO
    let received_peer_info = PeerInfo::decode(&mut &*rec_data).unwrap(); //TODO

    let send_data = pi.encode();
    let send_len_bytes = (send_data.len() as u32).to_le_bytes();
    stream.write_all(&send_len_bytes).await?;
    stream.write_all(&pi.encode()).await?;
    stream.flush().await?;
    Ok((stream, received_peer_info))
}
