use ethers::{
    prelude::*,
    abi::AbiDecode, 
    providers::Middleware,
    core::types::{ Filter, U256, Address},
};
use ethers_providers::{Provider, Ws};
use tokio::sync::{mpsc, Mutex, RwLock};
use std::{collections::{HashSet, HashMap},sync::Arc};
use once_cell::sync::Lazy;
use crate::RoundRobin;

/// Global provider cache: one provider per WebSocket URL
static PROVIDERS: Lazy<Mutex<HashMap<String, Arc<Provider<Ws>>>>> = 
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Struct representing a decoded ERC-20 Transfer event.
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
async fn get_provider(wss: &str) -> Result<Arc<Provider<Ws>>, Box<dyn std::error::Error + Send + Sync>> {
    let mut providers = PROVIDERS.lock().await;
    
    // Check if we already have a provider for this URL
    if let Some(provider) = providers.get(wss) {
        println!("♻️  Reusing existing provider for {}", wss);
        return Ok(Arc::clone(provider));
    }
    
    // Create new provider if not cached
    println!("🔌 Creating new WebSocket connection to {}", wss);
    let ws = Ws::connect(wss).await
        .map_err(|e| format!("WebSocket connection failed: {}", e))?;
    let provider = Arc::new(Provider::new(ws));
    
    // Cache it for future use
    providers.insert(wss.to_string(), Arc::clone(&provider));
    println!("✅ Provider created and cached for {}", wss);
    
    Ok(provider)
}

/// Remove a dead provider from cache (called when connection fails)
async fn remove_provider(wss: &str) {
    let mut providers = PROVIDERS.lock().await;
    if providers.remove(wss).is_some() {
        println!("🗑️  Removed dead provider for {} from cache", wss);
    }
}

/// Handle multiple WebSocket URLs with round-robin load balancing and retry
pub async fn get_transfer_events_with_load_balancer(
    selector: Arc<RoundRobin<String>>,
    contract_address: &str,
    decimal: u8,
    explorer: Option<String>,
    sent_notifications: Arc<RwLock<HashSet<String>>>,
    tx: mpsc::Sender<TransferEvent>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if selector.is_empty() {
        return Err("No RPC URLs provided".into());
    }
    
    let max_retries = selector.len() * 2; // Try each endpoint at least twice
    
    for attempt in 0..max_retries {
        let current_wss = match selector.next() {
            Some(wss) => wss,
            None => return Err("No URLs available".into()),
        };
        let endpoint_num = (attempt % selector.len()) + 1;
        
        println!("[Attempt {}/{}] Trying endpoint {}/{}: {}", 
                 attempt + 1, max_retries, endpoint_num, selector.len(), current_wss);
        
        match get_transfer_events(
            &current_wss,
            contract_address,
            decimal,
            explorer.clone(),
            Arc::clone(&sent_notifications),
            tx.clone(),
        ).await {
            Ok(_) => {
                // Connection ended normally, try next endpoint
                println!("⚠️  Connection to {} ended, rotating to next endpoint...", current_wss);
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                continue;
            }
            Err(e) => {
                eprintln!("❌ Error on {}: {}. Trying next endpoint...", current_wss, e);
                // Note: We keep the provider in cache - it might work on next attempt
                // If you want to force fresh connections on error, uncomment:
                // remove_provider(&current_wss).await;
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                continue;
            }
        }
    }
    
    Err("All endpoints exhausted after multiple retries".into())
}

/// Listens for Transfer events on the given contract and sends them through the channel.
/// - `rpc`: WebSocket RPC URL for the blockchain node.
/// - `contract_address`: Address of the token contract to monitor.
/// - `decimal`: Number of decimals for the token (for human-readable value).
/// - `explorer`: Optional block explorer URL for formatting links.
/// - `tx`: Channel sender to forward decoded TransferEvent structs.
pub async fn get_transfer_events(
    wss: &str, 
    contract_address: &str, 
    decimal: u8, 
    explorer: Option<String>, 
    sent_notifications: Arc<RwLock<HashSet<String>>>,
    tx: mpsc::Sender<TransferEvent>
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let provider = get_provider(wss).await?;
    let client = provider;
    
    // Build a filter for the Transfer event of the specified contract.
    println!("📋 Parsing contract address: {}", contract_address);
    let contract_addr = contract_address.parse::<Address>()
        .map_err(|e| format!("Invalid contract address '{}': {}", contract_address, e))?;
    
    let filter = Filter::new()
        .address(contract_addr)
        .event("Transfer(address,address,uint256)");
    
    println!("📡 Subscribing to Transfer events on {}...", wss);
    // Subscribe to the event logs using the filter.
    let mut stream = match client.subscribe_logs(&filter).await {
        Ok(s) => {
            println!("✅ Successfully subscribed! Listening for events on {}...", wss);
            s
        }
        Err(e) => {
            // Remove dead provider from cache on subscription failure
            remove_provider(wss).await;
            return Err(format!("Subscription failed: {}", e).into());
        }
    };

    // Process each log entry as it arrives.
    while let Some(log) = stream.next().await {
        // Safely extract transaction data with error handling
        if log.topics.len() < 3 {
            eprintln!("⚠️  [{}] Invalid log: not enough topics", wss);
            continue; // Skip this event, keep listening
        }
        
        let from: Address = Address::from(log.topics[1]);
        let to: Address = Address::from(log.topics[2]);
        
        let value: U256 = match U256::decode(log.data.as_ref()) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("⚠️  [{}] Failed to decode value: {}. Skipping event.", wss, e);
                continue; // Skip this event, keep listening
            }
        };
        
        let tx_hash: H256 = log.transaction_hash.unwrap_or_default();
        let notification_key = format!("{}:{}:{}:{}", from, to, value, tx_hash);

        // Quick check to skip already processed events (read lock - fast path)
        {
            let cache = sent_notifications.read().await;
            if cache.contains(&notification_key) {
                continue; // Already processed, skip
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
            eprintln!("❌ [{}] Channel closed, ending stream", wss);
            break; // Channel closed, exit gracefully
        }
        
        // Only mark as sent AFTER successful send to channel
        {
            let mut cache = sent_notifications.write().await;
            cache.insert(notification_key);
            
            // Prevent cache from growing too large
            if cache.len() > 10000 {
                cache.clear();
            }
        }
        
        println!("🔍 [{}] Detected transaction: {}", wss, tx_hash);
    }
    
    // Stream ended - remove provider from cache so it reconnects fresh
    println!("⚠️  Event stream ended for {}", wss);
    remove_provider(wss).await;
    Ok(())
}


    