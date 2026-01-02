use tracking_transfer_token_bot::{get_event, get_config, send_message_to_telegram, solana_adapter};
use tokio::{sync::{mpsc, Mutex}, select, signal::ctrl_c, time::{sleep, Duration, interval}};
use std::{collections::HashSet, sync::{Mutex as StdMutex,Arc}, str::FromStr};
use solana_sdk::pubkey::Pubkey;
use tracking_transfer_token_bot::RoundRobin;

#[tokio::main(flavor = "multi_thread", worker_threads = 8)]
pub async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create a channel with larger buffer for better throughput
    let (tx, rx) = mpsc::channel::<get_event::TransferEvent>(200);

    // Load all blockchain configurations from config.json.
    let configs = get_config::load_all_configs()?;
    if configs.is_empty() {
        eprintln!("No configurations found in config.json");
        return Ok(());
    }

    // Spawn a producer task for each config entry with auto-reconnect
    for (_, entry) in configs {
        let chain_type = entry.chain_type.as_deref().unwrap_or("evm");
        
        match chain_type {
            "solana" => {
                // Spawn Solana listener
                let urls = entry.get_urls();
                let rpc_url = urls.first().cloned().unwrap_or_default();
                // For Solana, url can be either https:// (for RPC) or wss:// (for WebSocket)
                // We need WebSocket for subscriptions, so convert if needed
                let ws_url = if rpc_url.starts_with("wss://") || rpc_url.starts_with("ws://") {
                    rpc_url.clone()
                } else {
                    rpc_url.replace("https://", "wss://").replace("http://", "ws://")
                };
                
                let mint = entry.address.clone();
                let name = entry.name.clone();
                let decimal = entry.decimal;
                let explorer = entry.explorer.clone();
                
                println!("[{}] Solana config: RPC={}, WS={}, Mint={}", name, rpc_url, ws_url, mint);
                
                // Create persistent caches that survive reconnections
                let processed_txs = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::<String, bool>::new()));
                let verified_accounts = Arc::new(StdMutex::new(HashSet::<String>::new()));
                let sent_notifications = Arc::new(StdMutex::new(HashSet::<String>::new()));
                
                tokio::spawn(async move {
                    loop {
                        println!("[{}] Starting Solana event listener...", name);
                        
                        // Parse token program ID (standard SPL Token)
                        let token_program_id = Pubkey::from_str("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA")
                            .expect("Invalid token program ID");
                        
                        match solana_adapter::get_detail_tx(
                            rpc_url.clone(),
                            ws_url.clone(),
                            mint.clone(),
                            decimal,
                            explorer.clone(),
                            token_program_id,
                            Arc::clone(&processed_txs),
                            Arc::clone(&verified_accounts),
                            Arc::clone(&sent_notifications),
                        ).await {
                            Ok(_) => println!("[{}] Solana stream ended normally", name),
                            Err(e) => eprintln!("[{}] Solana Error: {}. Reconnecting in 5s...", name, e),
                        }
                        
                        sleep(Duration::from_secs(5)).await;
                    }
                });
            }
            "evm" | _  => {
                // Spawn EVM listener (default)
                let tx = tx.clone();
                let rpc_urls = entry.get_urls();
                
                if rpc_urls.is_empty() {
                    eprintln!("[{}] No RPC URLs configured, skipping", entry.name);
                    continue;
                }
                
                let contract_address = entry.address.clone();
                let name = entry.name.clone();
                let decimal = entry.decimal;
                let explorer = entry.explorer.clone();
                
                // Create persistent resources that survive reconnections
                let selector = Arc::new(RoundRobin::new(rpc_urls.clone()));
                let sent_notifications = Arc::new(Mutex::new(HashSet::<String>::new()));
                
                println!("[{}] EVM config: {} WebSocket endpoint(s), Contract={}", 
                         name, rpc_urls.len(), contract_address);
                
                tokio::spawn(async move {
                    loop {
                        println!("[{}] Starting EVM event listener with {} endpoint(s)...", name, selector.len());
                        let sent_notifications = Arc::clone(&sent_notifications);
                        
                        match get_event::get_transfer_events_with_load_balancer(
                            Arc::clone(&selector),
                            &contract_address,
                            decimal,
                            explorer.clone(),
                            sent_notifications,
                            tx.clone()
                        ).await {
                            Ok(_) => println!("[{}] All EVM endpoints exhausted", name),
                            Err(e) => eprintln!("[{}] EVM Error: {}. Reconnecting in 5s...", name, e),
                        }
                        sleep(Duration::from_secs(5)).await;
                    }
                });
            }
        }
    }

    // Spawn multiple consumer workers for parallel processing
    let num_workers = 8;
    let rx = Arc::new(Mutex::new(rx));
    
    // Shared deduplication cache across all workers to prevent duplicates
    let seen_hashes = Arc::new(StdMutex::new(HashSet::<String>::new()));
    
    for worker_id in 0..num_workers {
        let rx = Arc::clone(&rx);
        let seen_hashes = Arc::clone(&seen_hashes);
        tokio::spawn(async move {
            let mut batch: Vec<get_event::TransferEvent> = Vec::new();
            let mut batch_interval = interval(Duration::from_secs(2));
            batch_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            
            loop {
                select! {
                    // Receive events and add to batch
                    event = async {
                        let mut guard = rx.lock().await;
                        guard.recv().await
                    } => {
                        let Some(event) = event else {
                            // Channel closed, send remaining batch and exit
                            if !batch.is_empty() {
                                send_batch(&mut batch, worker_id).await;
                            }
                            break;
                        };
                        
                        let tx_hash_str = format!("{:#x}", event.tx_hash);
                        
                        // Deduplication: skip if already processed (shared across all workers)
                        {
                            let mut cache = seen_hashes.lock().unwrap();
                            if cache.contains(&tx_hash_str) {
                                continue;
                            }
                            cache.insert(tx_hash_str);
                            
                            // Limit dedup cache size
                            if cache.len() > 1000 {
                                cache.clear();
                            }
                        }
                        
                        batch.push(event);
                        
                        // If batch is full, send immediately
                        if batch.len() >= 5 {
                            send_batch(&mut batch, worker_id).await;
                        }
                    }
                    // Send batch periodically even if not full
                    _ = batch_interval.tick() => {
                        if !batch.is_empty() {
                            send_batch(&mut batch, worker_id).await;
                        }
                    }
                }
            }
        });
    }

    // Drop the original sender so channel can close when all producers finish
    drop(tx);

    // Graceful shutdown on Ctrl+C
    select! {
        _ = ctrl_c() => {
            println!("\n🛑 Ctrl+C received, shutting down gracefully...");
            println!("⏳ Waiting for workers to finish processing...");
            
            // Give workers time to finish current batches
            sleep(Duration::from_secs(3)).await;
            
            println!("✅ Shutdown complete");
        }
    }
    
    Ok(())
}

/// Helper function to send a batch of events
async fn send_batch(batch: &mut Vec<get_event::TransferEvent>, worker_id: usize) {
    if batch.is_empty() {
        return;
    }
    
    println!("[Worker {}] Processing batch of {} events", worker_id, batch.len());
    
    for event in batch.drain(..) {
        let message = send_message_to_telegram::Message {
            from: event.from,
            to: event.to,
            value: event.value,
            tx_hash: event.tx_hash,
            explorer: event.explorer.clone(),
        };

        // Retry logic with exponential backoff
        let mut retry_count = 0;
        let max_retries = 3;
        
        while retry_count < max_retries {
            match send_message_to_telegram::send_message(&message).await {
                Ok(_) => break,
                Err(e) => {
                    retry_count += 1;
                    if retry_count < max_retries {
                        eprintln!("[Worker {}] Send failed (attempt {}/{}): {}. Retrying...", 
                                  worker_id, retry_count, max_retries, e);
                        sleep(Duration::from_millis(500 * retry_count as u64)).await;
                    } else {
                        eprintln!("[Worker {}] Failed to send message after {} attempts: {}", 
                                  worker_id, max_retries, e);
                    }
                }
            }
        }
        
        // Rate limiting: small delay between messages
        sleep(Duration::from_millis(100)).await;
    }
}