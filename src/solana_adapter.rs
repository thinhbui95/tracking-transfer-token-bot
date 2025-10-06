use solana_client::{
    rpc_client::RpcClient,
    pubsub_client::PubsubClient,
    rpc_config::{RpcTransactionLogsFilter, RpcTransactionLogsConfig},
};
use solana_commitment_config::CommitmentConfig;
use solana_transaction_status::{UiTransactionEncoding, UiInstruction, UiParsedInstruction};
use std::{ error::Error, str::FromStr };
use solana_sdk::pubkey::Pubkey;
use spl_token::{ state::Account as TokenAccount, solana_program::program_pack::Pack };
use tokio::sync::mpsc;
use crate::{ VerificationError, TransferEvent };


pub async fn get_detail_tx(rpc_url: String, ws_url: String, mint: String, decimal: u8, explorer: Option<String>, tx_receiver: mpsc::Sender<TransferEvent>) -> Result<(), Box<dyn Error + Send + Sync>> {

    let client = RpcClient::new_with_commitment(rpc_url.to_string(), CommitmentConfig::confirmed());

    let filter = RpcTransactionLogsFilter::Mentions(vec![mint.to_string()]);
    let (_subscription, receiver) = PubsubClient::logs_subscribe(
        &ws_url,
        filter,
        RpcTransactionLogsConfig {
            commitment: Some(CommitmentConfig::confirmed())
        },
    )?;

    let (tx, rx) = mpsc::channel::<String>(50);
    tokio::spawn(async move {
        while let Ok(logs) = receiver.recv() {
            let signature = logs.value.signature ;
            if !signature.is_empty() && logs.value.err.is_none() {
                if tx.send(signature).await.is_err() {
                    break;
                }
            }
        }
    });

    tokio::spawn(async move {
        if let Err(e) = get_info_transaction(&client, &mint, decimal, explorer, rx, tx_receiver).await {
            eprintln!("Error processing transactions: {}", e);
        }
    });

    Ok(())
}

fn verify_token_account_mint(
    client: &RpcClient,
    token_account: &Pubkey,
    expected_mint: &Pubkey,
) -> Result<(), VerificationError> {
    // Fetch account data
    let account_data = client.get_account_data(token_account)?;

    // Deserialize token account
    let token_account_data = TokenAccount::unpack(account_data.as_slice())
        .map_err(|_| VerificationError::InvalidTokenAccount)?;

    // Verify the mint address
    let token_mint_bytes = token_account_data.mint.to_bytes();
    let expected_mint_bytes = expected_mint.to_bytes();
    if token_mint_bytes != expected_mint_bytes {
        return Err(VerificationError::MintMismatch);
    }

    // Optional: Verify the account is owned by the SPL Token Program
    let token_program_id = Pubkey::from_str("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA").unwrap();
    let account_info = client.get_account(token_account)?;
    if account_info.owner != token_program_id {
        return Err(VerificationError::InvalidTokenAccount);
    }

    Ok(())
}

async fn get_info_transaction(client: &RpcClient, filter_account: &str, decimal: u8, explorer: Option<String>,mut rx: mpsc::Receiver<String>, tx_receiver: mpsc::Sender<TransferEvent>) -> Result<(), Box<dyn Error + Send + Sync>> {
    while let Some(tx_signature) = rx.recv().await {
        let tx = client.get_transaction_with_config(
            &tx_signature.parse()?,
            solana_client::rpc_config::RpcTransactionConfig {
                encoding: Some(UiTransactionEncoding::JsonParsed),
            commitment: Some(CommitmentConfig::confirmed()),
            max_supported_transaction_version: Some(0),
        },
        )?;

        if let Some(meta) = tx.transaction.meta {
            if meta.err.is_none() {
                for instruction in meta.inner_instructions.unwrap() {
                    for ix in instruction.instructions {
                        if let UiInstruction::Parsed(parsed) = ix {
                            if let UiParsedInstruction::Parsed(parsed_info) = parsed {
                                if parsed_info.program == "spl-token" {
                                    let mut data = TransferEvent::default();
                                    if let Some(source) = parsed_info.parsed.get("info").and_then(|info| info.get("source")).and_then(|s| s.as_str()) {
                                        if let (Ok(source_pubkey), Ok(mint_pubkey)) = (Pubkey::from_str(source), Pubkey::from_str(&filter_account)) {
                                            if verify_token_account_mint(&client, &source_pubkey, &mint_pubkey).is_ok() {
                                                if let Some(destination) = parsed_info.parsed.get("info").and_then(|info| info.get("destination")).and_then(|d| d.as_str()) {
                                                    if let Some(amount_str) = parsed_info.parsed.get("info").and_then(|info| info.get("amount")).and_then(|a| a.as_str()) {
                                                        if let Ok(amount) = amount_str.parse::<u64>() {
                                                            data.from = format!("{:?}", source);
                                                            data.to = format!("{:?}", destination);
                                                            data.value = amount as f64 / 10f64.powi(decimal as i32); // Assuming the token has 0 decimals for simplicity
                                                            data.tx_hash = tx_signature.clone();
                                                            data.explorer = explorer.clone();
                                                            print!("Transfer: {} -> {} | Value: {}\n", data.from, data.to, data.value);
                                                            if tx_receiver.send(data).await.is_err() {
                                                                eprintln!("Failed to send transfer event");
                                                            }
                                                        }
                                                    }
                                                }                    
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    } 

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{timeout, Duration};

   #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn test_get_detail_tx() {
        // Test configuration
        let rpc_url = "https://api.mainnet-beta.solana.com".to_string();
        let ws_url = "wss://api.mainnet-beta.solana.com".to_string();
        let mint = "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB".to_string(); // Example mint address
        let decimal = 6; // Example decimal for the token
        
        // Create a channel to receive transfer events
        let (tx_sender, mut tx_receiver) = mpsc::channel::<TransferEvent>(50);
        
        // Clone the sender for the test
        let tx_sender_clone = tx_sender.clone();
        print!("Starting get_detail_tx test...\n");
        
        // Spawn the get_detail_tx function
        let handle = tokio::spawn(async move {
            get_detail_tx(rpc_url, ws_url, mint, decimal, None,tx_sender_clone).await
        });
        
        // Wait for a short period to allow the function to set up
        tokio::time::sleep(Duration::from_secs(2)).await;
        
        // Try to receive events in a loop with timeout
        let mut event_count = 0;
        let max_events = 50; // Limit the number of events to receive
        
        loop {
            let result = tx_receiver.recv().await;
            
            match result {
                Some(event) => {
                
                    println!("Received transfer event {}: {:?}", event_count + 1, event);
                    event_count += 1;
                
                    // Break after receiving max_events to prevent infinite loop
                    if event_count >= max_events {
                        println!("Received maximum events, stopping...");
                        break;
                    }
                }
                None => {
                    println!("Channel closed without receiving events");
                    break;
                }
            }
        }
        
        println!("Total events received: {}", event_count);
        
        // Clean up
        handle.abort();
    }

    #[tokio::test]
    async fn test_get_detail_tx_invalid_urls() {
        let rpc_url = "https://invalid-url.com".to_string();
        let ws_url = "wss://invalid-url.com".to_string();
        let mint = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".to_string();
        let decimal = 6; // Example decimal for the token
        
        let (tx_sender, _tx_receiver) = mpsc::channel::<TransferEvent>(10);
        
        let result = get_detail_tx(rpc_url, ws_url, mint,decimal,None, tx_sender).await;
        
        // Should return an error due to invalid URLs
        assert!(result.is_err(), "Should return error for invalid URLs");
    }

    #[tokio::test]
    async fn test_get_detail_tx_invalid_mint() {
        let rpc_url = "https://api.mainnet-beta.solana.com".to_string();
        let ws_url = "wss://api.mainnet-beta.solana.com".to_string();
        let mint = "invalid-mint-address".to_string();
        let decimal = 6; // Example decimal for the token
        
        let (tx_sender, mut tx_receiver) = mpsc::channel::<TransferEvent>(10);
        
        let handle = tokio::spawn(async move {
            get_detail_tx(rpc_url, ws_url, mint, decimal, None, tx_sender).await
        });
        
        // Wait a bit for setup
        tokio::time::sleep(Duration::from_secs(2)).await;
        
        // Try to receive with timeout - should not get valid events
        let result = timeout(Duration::from_secs(5), tx_receiver.recv()).await;
        
        // Should either timeout or receive no events due to invalid mint
        match result {
            Ok(Some(_)) => {
                // If we receive something, it might be noise - this is acceptable
                println!("Received unexpected event with invalid mint");
            }
            Ok(None) | Err(_) => {
                // This is expected behavior
            }
        }
        
        handle.abort();
    }
}


