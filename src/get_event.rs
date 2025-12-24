use std::sync::Arc;
use ethers::{abi::AbiDecode, providers::Middleware};
use ethers::core::types::{ Filter, U256, Address};
use ethers_providers::Provider;
use ethers_providers::Ws;
use ethers::prelude::*;
use tokio::sync::mpsc;

// Struct representing a decoded ERC-20 Transfer event.
#[derive(Debug)]
pub struct TransferEvent {
    pub from: Address,
    pub to: Address,
    pub value: f64, // Human-readable value after applying decimals
    pub tx_hash: H256,
    pub explorer: Option<String>,
}


// Helper function to create a WebSocket provider for the given URL.
async fn get_provider(url: &str) -> Provider<Ws> {
    let ws = Ws::connect(url).await.expect("Failed to connect to WebSocket");
    Provider::new(ws)
}

/// Listens for Transfer events on the given contract and sends them through the channel.
/// - `rpc`: WebSocket RPC URL for the blockchain node.
/// - `contract_address`: Address of the token contract to monitor.
/// - `decimal`: Number of decimals for the token (for human-readable value).
/// - `explorer`: Optional block explorer URL for formatting links.
/// - `tx`: Channel sender to forward decoded TransferEvent structs.
pub async fn get_transfer_events(rpc: &str, contract_address: &str, decimal: u8, explorer: Option<String>, tx: mpsc::Sender<TransferEvent>) -> Result<(), Box<dyn std::error::Error>> {
    let provider = get_provider(rpc).await;
    let client = Arc::new(provider);
    // Build a filter for the Transfer event of the specified contract.
    let filter = Filter::new()
        .address(contract_address.parse::<Address>().unwrap())
        .event("Transfer(address,address,uint256)");
    
    // Subscribe to the event logs using the filter.
    let mut stream = client.subscribe_logs(&filter)
        .await
        .unwrap();

    // Process each log entry as it arrives.
    while let Some(log) = stream.next().await {
        // topics[0] is the event signature, topics[1] is from, topics[2] is to
        let from: Address = Address::from(log.topics[1]);
        let to: Address = Address::from(log.topics[2]);
        let value: U256 = U256::decode(log.data.as_ref()).unwrap();
        let tx_hash: H256 = log.transaction_hash.unwrap_or_default();

        // Convert value to human-readable float using the token's decimals.
        let transfer_event = TransferEvent {
            from,
            to,
            value: value.as_u128() as f64 / 10f64.powi(decimal as i32),
            tx_hash,
            explorer:explorer.clone()
        };

        // Send the transfer event to the channel
        if tx.send(transfer_event).await.is_err() {
            eprintln!("Failed to send transfer event");
        }
    }
    Ok(())
}


    