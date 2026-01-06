use thiserror::Error;
use serde::{ Deserialize , Serialize };
use ethers::core::types::Address;
use ethers::prelude::*;

#[derive(Error, Debug)]
pub enum VerificationError {
    #[error("RPC error: {0}")]
    RpcError(#[from] solana_client::client_error::ClientError),
    #[error("Invalid token account data")]
    InvalidTokenAccount,
    #[error("Token account does not belong to the specified mint")]
    MintMismatch,
}

#[derive(Debug)]
pub struct TransferEvent {
    pub from: String,
    pub to: String,
    pub value: f64,
    pub tx_hash: String,
    // Optional field for explorer URL
    pub explorer: Option<String>,
}

impl Default for TransferEvent {
    fn default() -> Self {
        TransferEvent {
            from: String::new(),
            to: String::new(),
            value: 0.0,
            tx_hash: String::new(),
            explorer: None,
        }
    }
}

/// Struct representing a configuration entry for a blockchain network.
#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    pub name: String,                // Name of the network (e.g., "BSC Mainnet")
    pub wss: String,                 // WebSocket RPC URL
    pub rpc: String,                 // HTTP RPC URL
    pub address: String,             // Token contract address
    pub decimal: u8,                 // Number of decimals for the token
    pub explorer: Option<String>,    // Optional field for explorer URL
}

/// Struct representing the message to be sent to Telegram.
#[derive(Debug)]
pub struct Message {
    pub from: String,
    pub to: String,
    pub value: f64,
    pub tx_hash: String,
    // Optional field for explorer URL
    pub explorer: Option<String>,
}

/// Enum representing different types of blockchain transfers
#[derive(Debug)]
pub enum TransferType {
    EVM {
        from: Address,
        to: Address,
        value: f64,
        tx_hash: H256,
    },
    Solana {
        from: String,
        to: String,
        amount: f64,
        tx_signature: String,
    },
}

/// Struct representing the message to be sent to Telegram.
#[derive(Debug)]
pub struct MessageToSend {
    pub transfer: TransferType,
    // Optional field for explorer URL
    pub explorer: Option<String>,
}