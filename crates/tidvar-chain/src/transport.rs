use crate::{
    ChainError,
    service::{BlockId, BlockRange},
};
use prost::Message;

pub const MAX_RANGE_BLOCKS: u64 = 128;
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

pub fn frame(payload: &[u8]) -> Result<Vec<u8>, ChainError> {
    let size = u32::try_from(payload.len()).map_err(|_| ChainError::Limit)?;
    let mut result = Vec::with_capacity(5 + payload.len());
    result.push(0);
    result.extend_from_slice(&size.to_be_bytes());
    result.extend_from_slice(payload);
    Ok(result)
}
pub fn request<T: Message>(value: &T) -> Result<Vec<u8>, ChainError> {
    frame(&value.encode_to_vec())
}
pub fn tree_request(height: u64) -> Result<Vec<u8>, ChainError> {
    request(&BlockId {
        height,
        hash: Vec::new(),
    })
}
pub fn range_request(start: u64, end: u64) -> Result<Vec<u8>, ChainError> {
    if end < start || end - start >= MAX_RANGE_BLOCKS {
        return Err(ChainError::Limit);
    }
    request(&BlockRange {
        start: Some(BlockId {
            height: start,
            hash: Vec::new(),
        }),
        end: Some(BlockId {
            height: end,
            hash: Vec::new(),
        }),
        pool_types: Vec::new(),
    })
}
pub fn decode_frames(bytes: &[u8]) -> Result<Vec<Vec<u8>>, ChainError> {
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(ChainError::Limit);
    }
    let mut position = 0;
    let mut values = Vec::new();
    let mut status = None;
    while position < bytes.len() {
        let header = bytes
            .get(position..position + 5)
            .ok_or(ChainError::Message)?;
        let size =
            u32::from_be_bytes(header[1..5].try_into().map_err(|_| ChainError::Message)?) as usize;
        position += 5;
        let end = position.checked_add(size).ok_or(ChainError::Limit)?;
        let body = bytes.get(position..end).ok_or(ChainError::Message)?;
        position = end;
        match header[0] {
            0 if status.is_none() => values.push(body.to_vec()),
            128 if status.is_none() => {
                let tail = std::str::from_utf8(body).map_err(|_| ChainError::Message)?;
                status = tail.lines().find_map(|line| {
                    line.trim_end_matches('\r')
                        .strip_prefix("grpc-status: ")
                        .map(str::to_owned)
                });
                if status.as_deref() != Some("0") {
                    return Err(ChainError::Unavailable);
                }
            }
            _ => return Err(ChainError::Message),
        }
    }
    if status.as_deref() != Some("0") {
        return Err(ChainError::Message);
    }
    Ok(values)
}
pub fn unary<T: Message + Default>(bytes: &[u8]) -> Result<T, ChainError> {
    let mut frames = decode_frames(bytes)?;
    if frames.len() != 1 {
        return Err(ChainError::Message);
    }
    T::decode(frames.remove(0).as_slice()).map_err(|_| ChainError::Message)
}
pub fn successful(mut messages: Vec<u8>) -> Vec<u8> {
    let tail = b"grpc-status: 0\r\n";
    messages.push(128);
    messages.extend_from_slice(&(tail.len() as u32).to_be_bytes());
    messages.extend_from_slice(tail);
    messages
}
