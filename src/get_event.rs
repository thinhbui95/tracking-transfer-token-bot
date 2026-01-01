use std::sync::Arc;
use ethers::{abi::AbiDecode, providers::Middleware};
use ethers::core::types::{ Filter, U256, Address};
use ethers_providers::Provider;
use ethers_providers::Ws;
use ethers::prelude::*;
use tokio::sync::{mpsc, Mutex};
use std::collections::{HashSet, HashMap};
use once_cell::sync::Lazy;

/// Global provider cache: one provider per WebSocket URL
static PROVIDERS: Lazy<Mutex<HashMap<String, Arc<Provider<Ws>>>>> = 
    Lazy::new(|| Mutex::new(HashMap::new()));

// Struct representing a decoded ERC-20 Transfer event.
#[derive(Debug)]
pub struct TransferEvent {
    pub from: Address,
    pub to: Address,
    pub value: f64, // Human-readable value after applying decimals
    pub tx_hash: H256,
    pub explorer: Option<String>,
}

impl Default for TransferEvent {
    fn default() -> Self {
        TransferEvent {
            from: Address::zero(),
            to: Address::zero(),
            value: 0.0,
            tx_hash: H256::zero(),
            explorer: None,
        }
    }
}


/// Get or create a cached WebSocket provider for the given URL (singleton pattern per URL)
async fn get_provider(url: &str) -> Result<Arc<Provider<Ws>>, Box<dyn std::error::Error>> {
    let mut providers = PROVIDERS.lock().await;
    
    // Check if we already have a provider for this URL
    if let Some(provider) = providers.get(url) {
        println!("♻️  Reusing existing provider for {}", url);
        return Ok(Arc::clone(provider));
    }
    
    // Create new provider if not cached
    println!("🔌 Creating new WebSocket connection to {}", url);
    let ws = Ws::connect(url).await
        .map_err(|e| format!("WebSocket connection failed: {}", e))?;
    let provider = Arc::new(Provider::new(ws));
    
    // Cache it for future use
    providers.insert(url.to_string(), Arc::clone(&provider));
    println!("✅ Provider created and cached for {}", url);
    
    Ok(provider)
}

/// Remove a dead provider from cache (called when connection fails)
async fn remove_provider(url: &str) {
    let mut providers = PROVIDERS.lock().await;
    if providers.remove(url).is_some() {
        println!("🗑️  Removed dead provider for {} from cache", url);
    }
}

/// Listens for Transfer events on the given contract and sends them through the channel.
/// - `rpc`: WebSocket RPC URL for the blockchain node.
/// - `contract_address`: Address of the token contract to monitor.
/// - `decimal`: Number of decimals for the token (for human-readable value).
/// - `explorer`: Optional block explorer URL for formatting links.
/// - `tx`: Channel sender to forward decoded TransferEvent structs.
pub async fn get_transfer_events(
    rpc: &str, 
    contract_address: &str, 
    decimal: u8, 
    explorer: Option<String>, 
    sent_notifications: Arc<Mutex<HashSet<String>>>,
    tx: mpsc::Sender<TransferEvent>
) -> Result<(), Box<dyn std::error::Error>> {
    let provider = get_provider(rpc).await?;
    let client = provider;
    
    // Build a filter for the Transfer event of the specified contract.
    println!("📋 Parsing contract address: {}", contract_address);
    let contract_addr = contract_address.parse::<Address>()
        .map_err(|e| format!("Invalid contract address '{}': {}", contract_address, e))?;
    
    let filter = Filter::new()
        .address(contract_addr)
        .event("Transfer(address,address,uint256)");
    
    println!("📡 Subscribing to Transfer events on {}...", rpc);
    // Subscribe to the event logs using the filter.
    let mut stream = match client.subscribe_logs(&filter).await {
        Ok(s) => {
            println!("✅ Successfully subscribed! Listening for events on {}...", rpc);
            s
        }
        Err(e) => {
            // Remove dead provider from cache on subscription failure
            remove_provider(rpc).await;
            return Err(format!("Subscription failed: {}", e).into());
        }
    };

    // Process each log entry as it arrives.
    while let Some(log) = stream.next().await {
        // topics[0] is the event signature, topics[1] is from, topics[2] is to
        let from: Address = Address::from(log.topics[1]);
        let to: Address = Address::from(log.topics[2]);
        let value: U256 = U256::decode(log.data.as_ref()).unwrap();
        let tx_hash: H256 = log.transaction_hash.unwrap_or_default();
        let notification_key = format!("{}:{}:{}:{}", from, to, value, tx_hash);

        {
            let cache = sent_notifications.lock().await;
            if cache.contains(&notification_key) {
                continue; // Already sent, skip
            }
        }

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
        } else {
            println!("🔍 Detected transaction: {}", tx_hash);
            // Mark this notification as sent
            let mut cache = sent_notifications.lock().await;
            cache.insert(notification_key);

            // Prevent cache from growing too large
            if cache.len() > 10000 {
                cache.clear();
            }
        }
    }
    
    // Stream ended - remove provider from cache so it reconnects fresh
    println!("⚠️  Event stream ended for {}", rpc);
    remove_provider(rpc).await;
    Ok(())
}


    