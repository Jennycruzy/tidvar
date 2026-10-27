use crate::{
    ChainError,
    compact_formats::CompactBlock,
    service::{self, compact_tx_streamer_client::CompactTxStreamerClient},
    transport,
};
use prost::Message;
use std::{collections::BTreeSet, io::Cursor, time::Duration};
use tidvar_core::record::Network;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};
use zcash_primitives::transaction::Transaction;

pub const TESTNET_SERVER: &str = "https://testnet.zec.rocks:443";
pub const MAINNET_SERVER: &str = "https://zec.rocks:443";
pub fn server(network: Network) -> &'static str {
    match network {
        Network::Mainnet => MAINNET_SERVER,
        Network::Testnet => TESTNET_SERVER,
    }
}
/// The consensus branch id lightwalletd reports for the current tip.
fn expected_branch(network: Network) -> &'static [&'static str] {
    match network {
        Network::Mainnet => &["37a5165b", "77190ad9"],
        Network::Testnet => &["77190ad9"],
    }
}

#[derive(Clone)]
pub struct Client {
    inner: CompactTxStreamerClient<Channel>,
    network: Network,
}
impl Client {
    pub async fn connect(server: &str) -> Result<Self, ChainError> {
        Self::connect_to(server, Network::Testnet).await
    }
    pub async fn connect_to(server: &str, network: Network) -> Result<Self, ChainError> {
        if !server.starts_with("https://") {
            return Err(ChainError::Network);
        }
        let endpoint = Endpoint::from_shared(server.to_owned())
            .map_err(|_| ChainError::Unavailable)?
            .tls_config(ClientTlsConfig::new().with_webpki_roots())
            .map_err(|_| ChainError::Unavailable)?
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(30));
        let channel = endpoint
            .connect()
            .await
            .map_err(|_| ChainError::Unavailable)?;
        let mut client = Self {
            inner: CompactTxStreamerClient::new(channel).max_decoding_message_size(8 * 1024 * 1024),
            network,
        };
        client.info().await?;
        Ok(client)
    }
    pub async fn info(&mut self) -> Result<service::LightdInfo, ChainError> {
        let info = self
            .inner
            .get_lightd_info(service::Empty {})
            .await
            .map_err(|_| ChainError::Unavailable)?
            .into_inner();
        if info.chain_name != self.network.chain_name()
            || !expected_branch(self.network).contains(&info.consensus_branch_id.as_str())
        {
            return Err(ChainError::Network);
        }
        Ok(info)
    }
    pub async fn latest(&mut self) -> Result<service::BlockId, ChainError> {
        self.inner
            .get_latest_block(service::ChainSpec {})
            .await
            .map(|r| r.into_inner())
            .map_err(|_| ChainError::Unavailable)
    }
    pub async fn tree(&mut self, height: u64) -> Result<service::TreeState, ChainError> {
        let tree = self
            .inner
            .get_tree_state(service::BlockId {
                height,
                hash: Vec::new(),
            })
            .await
            .map_err(|_| ChainError::Unavailable)?
            .into_inner();
        if tree.network != self.network.chain_name() || tree.height != height {
            return Err(ChainError::Network);
        }
        Ok(tree)
    }
    pub async fn blocks(&mut self, start: u64, end: u64) -> Result<Vec<CompactBlock>, ChainError> {
        if end < start || end - start >= transport::MAX_RANGE_BLOCKS {
            return Err(ChainError::Limit);
        }
        let mut stream = self
            .inner
            .get_block_range(service::BlockRange {
                start: Some(service::BlockId {
                    height: start,
                    hash: Vec::new(),
                }),
                end: Some(service::BlockId {
                    height: end,
                    hash: Vec::new(),
                }),
                pool_types: Vec::new(),
            })
            .await
            .map_err(|_| ChainError::Unavailable)?
            .into_inner();
        let mut blocks = Vec::new();
        let mut expected = start;
        while let Some(block) = stream
            .message()
            .await
            .map_err(|_| ChainError::Unavailable)?
        {
            if block.height != expected || block.height > end {
                return Err(ChainError::Message);
            }
            blocks.push(block);
            expected = expected.checked_add(1).ok_or(ChainError::Limit)?;
        }
        if expected != end + 1 {
            return Err(ChainError::Message);
        }
        Ok(blocks)
    }
    pub async fn mempool(
        &mut self,
    ) -> Result<tonic::Streaming<service::RawTransaction>, ChainError> {
        self.inner
            .get_mempool_stream(service::Empty {})
            .await
            .map(|r| r.into_inner())
            .map_err(|_| ChainError::Unavailable)
    }
    pub async fn transaction(&mut self, txid: &str) -> Result<service::RawTransaction, ChainError> {
        let mut hash = hex::decode(txid).map_err(|_| ChainError::Message)?;
        if hash.len() != 32 {
            return Err(ChainError::Message);
        }
        hash.reverse();
        self.inner
            .get_transaction(service::TxFilter {
                block: None,
                index: 0,
                hash,
            })
            .await
            .map(|r| r.into_inner())
            .map_err(|error| {
                if error.code() == tonic::Code::NotFound {
                    ChainError::TransactionMissing
                } else {
                    ChainError::Unavailable
                }
            })
    }
    pub async fn broadcast(&mut self, raw: Vec<u8>) -> Result<String, ChainError> {
        self.info().await?;
        let tip = self.latest().await?;
        let txid = validate_on(&raw, tip.height, self.network)?;
        let result = self
            .inner
            .send_transaction(service::RawTransaction {
                data: raw,
                height: 0,
            })
            .await
            .map_err(|_| ChainError::Unavailable)?
            .into_inner();
        if result.error_code != 0 {
            return Err(ChainError::Message);
        }
        Ok(txid)
    }
    pub async fn relay(
        &mut self,
        method: &str,
        payload: &[u8],
        evidence: &BTreeSet<Vec<u8>>,
    ) -> Result<Vec<u8>, ChainError> {
        let mut out = Vec::new();
        match method {
            "GetLightdInfo" => out.extend(transport::request(&self.info().await?)?),
            "GetLatestBlock" => out.extend(transport::request(&self.latest().await?)?),
            "GetTreeState" => {
                let request = service::BlockId::decode(payload).map_err(|_| ChainError::Message)?;
                if !request.hash.is_empty() {
                    return Err(ChainError::Message);
                }
                out.extend(transport::request(&self.tree(request.height).await?)?);
            }
            "GetBlockRange" => {
                let range =
                    service::BlockRange::decode(payload).map_err(|_| ChainError::Message)?;
                let start = range.start.ok_or(ChainError::Message)?;
                let end = range.end.ok_or(ChainError::Message)?;
                if !start.hash.is_empty() || !end.hash.is_empty() || !range.pool_types.is_empty() {
                    return Err(ChainError::Message);
                }
                for block in self.blocks(start.height, end.height).await? {
                    out.extend(transport::request(&block)?);
                    if out.len() > transport::MAX_RESPONSE_BYTES {
                        return Err(ChainError::Limit);
                    }
                }
            }
            "GetTransaction" => {
                let request =
                    service::TxFilter::decode(payload).map_err(|_| ChainError::Message)?;
                if request.block.is_some()
                    || request.index != 0
                    || !evidence.contains(&request.hash)
                {
                    return Err(ChainError::Message);
                }
                let result = self
                    .inner
                    .get_transaction(request)
                    .await
                    .map_err(|_| ChainError::Unavailable)?
                    .into_inner();
                out.extend(transport::request(&result)?);
            }
            "SendTransaction" => {
                let request =
                    service::RawTransaction::decode(payload).map_err(|_| ChainError::Message)?;
                self.broadcast(request.data).await?;
                out.extend(transport::request(&service::SendResponse {
                    error_code: 0,
                    error_message: String::new(),
                })?);
            }
            _ => return Err(ChainError::Message),
        }
        Ok(transport::successful(out))
    }
}

pub fn validate_broadcast(raw: &[u8], tip: u64) -> Result<String, ChainError> {
    validate_on(raw, tip, Network::Testnet)
}
pub fn validate_on(raw: &[u8], tip: u64, network: Network) -> Result<String, ChainError> {
    let next = u32::try_from(tip.saturating_add(1)).map_err(|_| ChainError::Message)?;
    let branch = network.ironwood_branch(next).ok_or(ChainError::Network)?;
    let mut cursor = Cursor::new(raw);
    let transaction = Transaction::read(&mut cursor, branch).map_err(|_| ChainError::Message)?;
    if cursor.position() != raw.len() as u64 {
        return Err(ChainError::Message);
    }
    if transaction.consensus_branch_id() != branch || !transaction.version().valid_in_branch(branch)
    {
        return Err(ChainError::Network);
    }
    let expiry = u32::from(transaction.expiry_height());
    if expiry > 0 && tip >= u64::from(expiry) {
        return Err(ChainError::Expired);
    }
    Ok(transaction.txid().to_string())
}
