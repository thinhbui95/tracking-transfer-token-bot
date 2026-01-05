use solana_client::{
    pubsub_client::PubsubClient,
    rpc_config::{RpcTransactionLogsFilter, RpcTransactionLogsConfig},
    nonblocking::rpc_client::RpcClient as AsyncRpcClient,
};
use solana_commitment_config::CommitmentConfig;
use solana_transaction_status::{UiTransactionEncoding, UiInstruction, UiParsedInstruction};
use solana_sdk::pubkey::Pubkey;
use spl_token::{ state::Account as TokenAccount, solana_program::program_pack::Pack };
use teloxide::{requests::Requester, payloads::SendMessageSetters};
use dotenv::from_path;
use std::{ 
    error::Error, 
    str::FromStr, 
    sync::{Arc, Mutex}, 
    fs,
    collections::{HashMap, HashSet}
};
use once_cell::sync::OnceCell;
use tokio::{
    sync::{Semaphore, RwLock},
    time::{sleep, timeout, Duration},
};
use crate::send_message_to_telegram;
use crate::RoundRobin;
use crate::VerificationError;

/// Global singleton rpc, semaphore instance, initialized once on first use
static RPC_LIST: OnceCell<Arc<Vec<String>>> = OnceCell::new();
static RPC_CLIENT_SELECTOR: OnceCell<Arc<RoundRobin<Arc<AsyncRpcClient>>>> = OnceCell::new();
static REQUEST_SEMAPHORE: OnceCell<Arc<Semaphore>> = OnceCell::new();
static CHAT_ID: OnceCell<i64> = OnceCell::new();

/// Get or initialize the chat ID (singleton pattern)
fn get_chat_id() -> Result<i64, Box<dyn Error>> {
    CHAT_ID.get_or_try_init(|| {
        from_path("src/asset/.env").ok();
        let chat_id: i64 = std::env::var("TELEGRAM_CHAT_ID")
            .map_err(|_| "TELEGRAM_CHAT_ID not set")?.parse()
            .map_err(|_| "Invalid TELEGRAM_CHAT_ID")?;
        Ok(chat_id)
    }).map(|id| *id)
}

/// Get or initialize the RPC list (singleton pattern)
fn get_rpc_list() -> Result<Arc<Vec<String>>, Box<dyn Error>> {
    RPC_LIST.get_or_try_init(|| {
        let rpc_list_str = fs::read_to_string("src/asset/list_of_rpc.json")?;
        let rpc_list: Vec<String> = serde_json::from_str(&rpc_list_str)?;
        Ok(Arc::new(rpc_list))
    }).map(Arc::clone)
}

/// Get or initialize the RPC client selector (singleton pattern)
fn get_rpc_client_selector() -> Result<Arc<RoundRobin<Arc<AsyncRpcClient>>>, Box<dyn Error>> {
    RPC_CLIENT_SELECTOR.get_or_try_init(|| {
        let rpc_list = get_rpc_list()?;
        let clients: Vec<Arc<AsyncRpcClient>> = rpc_list
            .iter()
            .map(|url| {
                Arc::new(AsyncRpcClient::new_with_commitment(
                    url.clone(),
                    CommitmentConfig::confirmed(),
                ))
            })
            .collect();
        Ok(Arc::new(RoundRobin::new(clients)))
    }).map(Arc::clone)
}

/// Get or initialize the request semaphore (singleton pattern)
fn get_request_semaphore() -> Arc<Semaphore> {
    REQUEST_SEMAPHORE.get_or_init(|| {
        Arc::new(Semaphore::new(50)) // Increased to 50 concurrent requests
    }).clone()
}

/// Send Solana transfer notification to Telegram
async fn send_solana_transfer_to_telegram(
    source: &str,
    destination: &str,
    amount: f64,
    tx_signature: &str,
    explorer_url: Option<&str>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let explorer = explorer_url.unwrap_or("https://solscan.io");
    
    let source_link = format!(r#"<a href="{}/account/{}">{}...</a>"#, explorer, source, &source[..8]);
    let dest_link = format!(r#"<a href="{}/account/{}">{}...</a>"#, explorer, destination, &destination[..8]);
    let tx_link = format!(r#"<a href="{}/tx/{}">Detail</a>"#, explorer, tx_signature);

    // Load environment variables for chat ID
    let chat_id: i64 = get_chat_id().unwrap();

    // Get the shared bot instance (created only once)
    let bot = send_message_to_telegram::get_telegram_bot()?;

    let text = format!(
        "🟣 <b>Solana Transfer</b>\n\nFrom: {}\nTo: {}\n💰 Amount: {}\n📝 Tx: {}",
        source_link, dest_link, amount, tx_link
    );

    bot.send_message(teloxide::types::ChatId(chat_id), text)
        .parse_mode(teloxide::types::ParseMode::Html)
        .await
        .map_err(|e| format!("Failed to send: {}", e))?;
    
    Ok(())
}

pub async fn get_detail_tx(
    rpc_url: String, 
    ws_url: String, 
    mint: String, 
    decimal: u8, 
    explorer: Option<String>, 
    program_token_id: Pubkey,
    // Persistent caches that survive reconnections
    processed_txs: Arc<RwLock<HashMap<String, bool>>>,
    verified_accounts: Arc<Mutex<HashSet<String>>>,
    sent_notifications: Arc<Mutex<HashSet<String>>>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    // Use singleton RPC client selector and semaphore to avoid recreating on reconnections
    let rpc_client_selector = get_rpc_client_selector().unwrap_or_else(|_| {
        // Fallback: create a new selector with just the provided RPC URL
        let client = Arc::new(AsyncRpcClient::new_with_commitment(
            rpc_url.to_string(),
            CommitmentConfig::confirmed(),
        ));
        Arc::new(RoundRobin::new(vec![client]))
    });
    let semaphore = get_request_semaphore();
    
    let filter = RpcTransactionLogsFilter::Mentions(vec![mint.to_string()]);
    let (_subscription, receiver) = PubsubClient::logs_subscribe(
        &ws_url,
        filter,
        RpcTransactionLogsConfig {
            commitment: Some(CommitmentConfig::confirmed())
        },
    )?;
    
    // Process transactions with rate limiting
    while let Ok(logs) = receiver.recv() {
        let signature = logs.value.signature;
        if !signature.is_empty() && logs.value.err.is_none() {
            // Quick check for duplicates
            {
                let cache = processed_txs.read().await;
                if cache.contains_key(&signature) {
                    continue; // Skip already processed transactions
                }
            }
            
            let rpc_client_selector = Arc::clone(&rpc_client_selector);
            let mint = mint.clone();
            let program_token_id = program_token_id.clone();
            let semaphore = Arc::clone(&semaphore);
            let processed_txs = Arc::clone(&processed_txs);
            let verified_accounts = Arc::clone(&verified_accounts);
            let sent_notifications = Arc::clone(&sent_notifications);
            let explorer_clone = explorer.clone();
            let sig_clone = signature.clone();
                        
            // Spawn task with rate limiting
            tokio::spawn(async move {
                // Mark as processing
                {
                    let mut cache = processed_txs.write().await;
                    if cache.insert(sig_clone.clone(), true).is_some() {
                        return; // Already being processed
                    }
                    // Limit cache size
                    if cache.len() > 10000 {
                        cache.clear();
                    }
                }
                
                // Acquire permit before making RPC calls
                let _permit = semaphore.acquire().await.unwrap();
                
                // Get a client from round-robin pool (reuses connections)
                let client = rpc_client_selector.next().expect("No RPC clients available");
                
                // Use timeout and retry logic for better resilience
                if let Err(e) = get_info_transaction_with_retry(client, rpc_client_selector, &mint, decimal, &program_token_id, &sig_clone, verified_accounts, sent_notifications, explorer_clone).await {
                    eprintln!("Error processing transaction {}: {}", sig_clone, e);
                }
                
                // Permit automatically released when dropped
            });
        }
    }

    Ok(())
}

/// Get transaction info with retry logic and timeout
async fn get_info_transaction_with_retry(
    client: Arc<AsyncRpcClient>,
    rpc_client_selector: Arc<RoundRobin<Arc<AsyncRpcClient>>>,
    filter_account: &str,
    decimal: u8,
    token_program_id: &Pubkey,
    tx_signature: &str,
    verified_accounts: Arc<Mutex<HashSet<String>>>,
    sent_notifications: Arc<Mutex<HashSet<String>>>,
    explorer: Option<String>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let max_retries = 5;
    let request_timeout = Duration::from_secs(5);
    
    for attempt in 0..=max_retries {
        let current_client = if attempt == 0 {
            client.clone()
        } else {
            // Try a different RPC on retry
            rpc_client_selector.next().expect("No RPC clients available")
        };
        // Implement timeout for each attempt
        match timeout(
            request_timeout,
            get_info_transaction(current_client, filter_account, decimal, token_program_id, tx_signature, verified_accounts.clone(), sent_notifications.clone(), explorer.clone())
        ).await {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(e)) if attempt < max_retries => {
                eprintln!("Retry {}/{} for {}: {}", attempt + 1, max_retries, tx_signature, e);
                sleep(Duration::from_millis(50 * (attempt + 1) as u64)).await;
                continue;
            }
            Ok(Err(e)) => return Err(e),
            Err(_) if attempt < max_retries => {
                eprintln!("Timeout retry {}/{} for {}, trying next RPC...", attempt + 1, max_retries, tx_signature);
                // No need to force advance - next iteration will get next client automatically
                sleep(Duration::from_millis(50)).await;
                continue;
            }
            Err(_) => return Err("Request timeout".into()),
        }
    }
    
    Err("Max retries exceeded".into())
}

async fn get_info_transaction(
    client: Arc<AsyncRpcClient>, 
    filter_account: &str, 
    decimal: u8, 
    token_program_id: &Pubkey, 
    tx_signature: &str,
    verified_accounts: Arc<Mutex<HashSet<String>>>,
    sent_notifications: Arc<Mutex<HashSet<String>>>,
    explorer: Option<String>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let tx = client.get_transaction_with_config(
        &tx_signature.parse()?,
        solana_client::rpc_config::RpcTransactionConfig {
            encoding: Some(UiTransactionEncoding::JsonParsed),
            commitment: Some(CommitmentConfig::confirmed()),
            max_supported_transaction_version: Some(0),
        },
    ).await?;

    let meta = tx.transaction.meta.ok_or("No metadata")?;
    if meta.err.is_some() {
        return Ok(()); // Skip failed transactions
    }
    let mint_pubkey = Pubkey::from_str(filter_account)?;
    
    // Track processed transfers within this transaction to avoid duplicates
    let mut processed_transfers = HashSet::new();
    
    // Collect all instructions to process (both main and inner)
    let mut all_instructions = Vec::new();
    
    // Add main transaction instructions
    if let solana_transaction_status::EncodedTransaction::Json(ui_tx) = &tx.transaction.transaction {
        if let solana_transaction_status::UiMessage::Parsed(parsed_msg) = &ui_tx.message {
            for instruction in &parsed_msg.instructions {
                all_instructions.push(instruction.clone());
            }
        }
    }
    
    // Add inner instructions if they exist
    if meta.inner_instructions.is_some() {
        for instruction_group in  meta.inner_instructions.unwrap() {
            for ix in instruction_group.instructions {
                all_instructions.push(ix);
            }
        }
    }
    
    // Process all instructions
    for ix in all_instructions {
        if let UiInstruction::Parsed(UiParsedInstruction::Parsed(parsed_info)) = ix {
            // Only process SPL token programs
            if parsed_info.program != "spl-token" && parsed_info.program != "spl-token-2022" {
                continue;
            }
            
            // Extract transfer details
            let info = parsed_info.parsed.get("info");
            let source = info.and_then(|i| i.get("source")).and_then(|s| s.as_str());
            let destination = info.and_then(|i| i.get("destination")).and_then(|d| d.as_str());
            
            // Handle both "transfer" (uses "amount") and "transferChecked" (uses "tokenAmount.amount")
            let amount_str = info
                .and_then(|i| i.get("amount"))
                .and_then(|a| a.as_str())
                .or_else(|| {
                    info.and_then(|i| i.get("tokenAmount"))
                        .and_then(|ta| ta.get("amount"))
                        .and_then(|a| a.as_str())
                });
            
            if let (Some(source), Some(destination), Some(amount_str)) = (source, destination, amount_str) {
                let Ok(source_pubkey) = Pubkey::from_str(source) else { continue };
                let Ok(amount) = amount_str.parse::<u64>() else { continue };
                
                // Create unique key for this transfer within the transaction to prevent duplicates
                let transfer_key = format!("{}:{}:{}", source, destination, amount);
                if !processed_transfers.insert(transfer_key) {
                    // Already processed this exact transfer in this transaction, skip
                    continue;
                }
                
                // Check verification cache first (avoids expensive RPC calls)
                let source_cache_key = format!("{}:{}", source, filter_account);
                let destination_cache_key = format!("{}:{}", destination, filter_account);
                let is_verified = {
                    let cache = verified_accounts.lock().unwrap();
                    cache.contains(&source_cache_key) || cache.contains(&destination_cache_key)
                };
                
                if !is_verified {
                    // Verify and cache the result
                    if verify_token_account_mint(&client, &source_pubkey, &mint_pubkey, token_program_id).await.is_err() {
                        continue;
                    }
                    
                    let mut cache = verified_accounts.lock().unwrap();
                    cache.insert(source_cache_key);
                    cache.insert(destination_cache_key);
                    
                    // Prevent cache from growing too large
                    if cache.len() > 50000 {
                        cache.clear();
                    }
                }
                
                // Calculate amount value
                let amount_value = amount as f64 / 10f64.powi(decimal as i32);
                
                // Check if we already sent this notification (prevent duplicates during retries)
                let notification_key = format!("{}:{}:{}:{}", source, destination, amount, tx_signature);
                {
                    let cache = sent_notifications.lock().unwrap();
                    if cache.contains(&notification_key) {
                        continue; // Already sent, skip
                    }
                }
                
                // Send to Telegram
                if let Err(e) = send_solana_transfer_to_telegram(
                    source,
                    destination,
                    amount_value,
                    tx_signature,
                    explorer.as_deref(),
                ).await {
                    eprintln!("Failed to send Telegram notification: {}", e);
                } else {
                    // Mark as sent
                    let mut cache = sent_notifications.lock().unwrap();
                    cache.insert(notification_key);
                    
                    // Prevent cache from growing too large
                    if cache.len() > 10000 {
                        cache.clear();
                    }
                    println!("Detected transaction: {}", tx_signature);
                    println!("✅ Sent: {} -> {} | {} ", &source[..8], &destination[..8], amount_value);
                }
            }
        }
    }

    Ok(())
}


async fn verify_token_account_mint(
    client: &AsyncRpcClient,
    token_account: &Pubkey,
    expected_mint: &Pubkey,
    token_program_id: &Pubkey,
) -> Result<(), VerificationError> {
    // Fetch account info in one async call instead of two
    let account_info = client.get_account(token_account).await?;

    // Verify owner first (cheap check)
    if account_info.owner != *token_program_id {
        return Err(VerificationError::InvalidTokenAccount);
    }

    // Deserialize token account
    let token_account_data = TokenAccount::unpack(&account_info.data)
        .map_err(|_| VerificationError::InvalidTokenAccount)?;

    // Verify the mint address
    if token_account_data.mint != *expected_mint {
        return Err(VerificationError::MintMismatch);
    }

    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;
    use tokio::signal;

    // Test round-robin functionality
    #[test]
    fn test_round_robin_rotation() {
        let rpc_urls = vec![
            "https://rpc1.com".to_string(),
            "https://rpc2.com".to_string(),
            "https://rpc3.com".to_string(),
        ];
        let rr = RoundRobin::new(rpc_urls.clone());
        
        // Test that it rotates through all URLs
        assert_eq!(rr.next(), Some("https://rpc1.com".to_string()));
        assert_eq!(rr.next(), Some("https://rpc2.com".to_string()));
        assert_eq!(rr.next(), Some("https://rpc3.com".to_string()));
        assert_eq!(rr.next(), Some("https://rpc1.com".to_string())); // Should wrap around
        assert_eq!(rr.next(), Some("https://rpc2.com".to_string()));
    }

    // Test loading RPC list
    #[test]
    fn test_load_rpc_list() {
        let result = get_rpc_list();
        assert!(result.is_ok());
        let rpc_list = result.unwrap();
        assert!(!rpc_list.is_empty());
    }

    // This test subscribes to websocket and runs indefinitely - stop with Ctrl+C
    // Run with: cargo test --lib -- solana_adapter::tests::test_get_detail_tx_live --ignored --nocapture -- --test-threads=1
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    #[ignore]
    async fn test_get_detail_tx_live() {
        let rpc_url = "https://api.mainnet-beta.solana.com".to_string();
        let ws_url = "wss://api.mainnet-beta.solana.com".to_string();
        let mint = "GCdgrGgGcczEdfW2P6wLG8jjxzmhtdcPSJQhB2YmMZP9".to_string();
        let decimal = 6;
        let token_program_id = Pubkey::from_str("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA").unwrap();

        println!("Starting live transaction monitoring (press Ctrl+C to stop)...");
        
        // Create persistent caches for test
        let processed_txs = Arc::new(RwLock::new(HashMap::new()));
        let verified_accounts = Arc::new(Mutex::new(HashSet::new()));
        let sent_notifications = Arc::new(Mutex::new(HashSet::new()));
        
        // Spawn the transaction monitoring in a task
        let monitor_task = tokio::spawn(async move {
            if let Err(e) = get_detail_tx(rpc_url, ws_url, mint, decimal, None, token_program_id, processed_txs, verified_accounts, sent_notifications).await {
                eprintln!("Stream ended: {}", e);
            }
        });

        // Wait for Ctrl+C signal
        let _ = signal::ctrl_c().await;
        println!("\n\nCtrl+C received. Shutting down...");
        
        // Cancel the monitoring task
        monitor_task.abort();
        
        // Give it a moment to clean up
        sleep(Duration::from_millis(100)).await;
        println!("Test completed gracefully.");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn test_get_info_transaction() {
        let rpc_url = "https://api.mainnet-beta.solana.com".to_string();
        let mint = "SarosY6Vscao718M4A778z4CGtvcwcGef5M9MEH1LGL".to_string();
        let token_program_id = Pubkey::from_str("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA").unwrap();
        let decimal = 6;
        let tx_signature = "5RXcWBpMmY65wdXjFVkZttVGiRy5HwDUrKD1DQRFtwx1hctmxj3MqqydA2HiqKVLmXf9FZpUMsjpCqFFepJasMsj".to_string();
        let client = Arc::new(AsyncRpcClient::new_with_commitment(rpc_url.to_string(), CommitmentConfig::confirmed()));
        let verified_accounts = Arc::new(Mutex::new(HashSet::new()));
        let sent_notifications = Arc::new(Mutex::new(HashSet::new()));
        let explorer = Some("https://solscan.io".to_string());

        let result = get_info_transaction(client, &mint, decimal, &token_program_id, &tx_signature, verified_accounts, sent_notifications, explorer).await;
        assert!(result.is_ok());
    }
}
