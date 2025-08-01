use ethers::prelude::*;
use ethers_providers::Ws;
use ethers::core::types::{ Filter, U256, Address};
use tokio::sync::mpsc;
use tokio::select;
use tokio::signal::ctrl_c;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
pub async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create a channel for sending TransferEvent messages between producer and consumer tasks.
    let (tx, mut rx) = mpsc::channel::<get_event::TransferEvent>(5);

    // Load all blockchain configurations from config.json.
    let configs = get_config::Config::load_all_configs()?;
    if configs.is_empty() {
        eprintln!("No configurations found in config.json");
        return Ok(());
    }

    // Spawn a producer task for each config entry to listen for transfer events.
    for (_key, entry) in configs {
        let tx = tx.clone();
        let rpc_url = entry.url.clone();
        let contract_address = entry.address.clone();
        let name = entry.name.clone();
        let decimal = entry.decimal;
        let explorer = entry.explorer.clone();
        tokio::spawn(async move {
            if let Err(e) = get_event::get_transfer_events(&rpc_url, &contract_address, decimal, explorer, tx).await {
                println!("Error fetching events for {}: {}", name, e);
            }
        });
    }

    // Spawn a consumer task to receive events and send formatted messages to Telegram.
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            let message = send_message_to_telegram::Message {
                from: event.from,
                to: event.to,
                value: event.value,
                tx_hash: event.tx_hash,
                explorer: event.explorer.clone(), // Pass the explorer URL if available
            };

            // Call the send_message function to notify via Telegram.
            if let Err(e) = send_message_to_telegram::send_message(&message).await {
                println!("Error sending message: {}", e);
            }
        }
    });

    // Keep the main function alive and gracefully shut down on Ctrl+C.
    loop {
        select! {
            _ = ctrl_c() => {
                println!("Ctrl+C pressed, shutting down...");
                return Ok(());
            }
        }
    }
}

mod get_event {
    use std::sync::Arc;
    use ethers::{abi::AbiDecode, providers::Middleware};
    use super::*;

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
}

mod get_config {
    use std::fs;
    use std::collections::HashMap;
    use serde::{ Deserialize , Serialize };

    /// Struct representing a configuration entry for a blockchain network.
    #[derive(Debug, Deserialize, Serialize)]
    pub struct Config {
        pub name: String,                // Name of the network (e.g., "BSC Mainnet")
        pub url: String,                 // WebSocket RPC URL
        pub address: String,             // Token contract address
        pub decimal: u8,                 // Number of decimals for the token
        pub explorer: Option<String>,    // Optional field for explorer URL
    }

    impl Config {
        // Add a new config entry to config.json
        #[allow(dead_code)]
        pub fn add_config_to_file(key: String, config: Config) -> Result<(), Box<dyn std::error::Error>> {
            let path = "src/asset/config.json";
            // Load existing configs
            let mut configs: HashMap<String, Config> = if let Ok(content) = fs::read_to_string(path) {
                serde_json::from_str(&content)?
            } else {
                HashMap::new()
            };
            // Insert new config
            configs.insert(key, config);
            // Write back to file
            let new_content = serde_json::to_string_pretty(&configs)?;
            fs::write(path, new_content)?;
            Ok(())
        }

        /// Loads all config entries from config.json into a HashMap.
        pub fn load_all_configs() -> Result<std::collections::HashMap<String, Config>, Box<dyn std::error::Error>> {
            let config_str = fs::read_to_string("src/asset/config.json")?;
            let configs: std::collections::HashMap<String, Config> = serde_json::from_str(&config_str)?;
            Ok(configs)
        }
    }
}

mod send_message_to_telegram {
    use teloxide::{ Bot, requests::Requester, payloads::SendMessageSetters };
    use dotenv::from_path;
    use super::*;

    /// Struct representing the message to be sent to Telegram.
    #[derive(Debug)]
    pub struct Message {
        pub from: Address,
        pub to: Address,
        pub value: f64,
        pub tx_hash: H256,
        // Optional field for explorer URL
        pub explorer: Option<String>,
    }


    /// Sends a formatted message to a Telegram chat using the bot.
    /// The message includes clickable links for the from/to addresses and transaction hash.
    pub async fn send_message(message: &Message) -> Result<(), Box<dyn std::error::Error>> {
        // Load environment variables from the custom .env path
        from_path("src/asset/.env").ok();

        // Ensure the TELEGRAM_BOT_KEY is set
        if std::env::var("TELEGRAM_BOT_KEY").is_err() {
            return Err("TELEGRAM_BOT_KEY is not set".into());
        }

        let explorer_url = message.explorer.clone().unwrap_or_default();
        let from_link = format!(
            r#"<a href="{}/address/{:#x}">{}</a>"#, 
            explorer_url, message.from, message.from.to_string()
        );
        let to_link = format!(
            r#"<a href="{}/address/{:#x}">{}</a>"#, 
            explorer_url, message.to, message.to.to_string()
        );
        let tx_link = format!(
            r#"<a href="{}/tx/{:#x}">{}</a>"#, 
            explorer_url, message.tx_hash, "Detail"
        );

        // Get chat ID
        let chat_id: i64 = std::env::var("TELEGRAM_CHAT_ID")
            .map_err(|_| "TELEGRAM_CHAT_ID environment variable is not set")?
            .parse()
            .map_err(|_| "TELEGRAM_CHAT_ID is not a valid i64")?;

        let bot = Bot::new(&std::env::var("TELEGRAM_BOT_KEY").unwrap());

        // Send the message with HTML parse mode
        let text = format!(
            "Transfer Event:\nFrom: {}\nTo: {}\nValue: {}\nTx_Hash: {}",
            from_link,
            to_link,
            message.value,
            tx_link
        );
        bot.send_message(
            teloxide::types::ChatId(chat_id),
            text,
        )
        .parse_mode(teloxide::types::ParseMode::Html) // <--- This is required!
        .await
        .map_err(|e| format!("Failed to send message: {}", e))?;
        Ok(())
    }
}