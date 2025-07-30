use ethers::prelude::*;
use ethers_providers::Ws;
use ethers::core::types::{ Filter, U256, Address};
use tokio::sync::mpsc;
use tokio::select;
use tokio::signal::ctrl_c;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
pub async fn main() -> Result<(), Box<dyn std::error::Error>> {

    let (tx, mut rx) = mpsc::channel::<get_event::TransferEvent>(5);

    let configs = get_config::Config::load_all_configs()?;
    if configs.is_empty() {
        eprintln!("No configurations found in config.json");
        return Ok(());
    }

    // Spawn producer
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

    // Spawn consumer
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            let message = send_message_to_telegram::Message {
                from: event.from,
                to: event.to,
                value: event.value,
                tx_hash: event.tx_hash,
                explorer: event.explorer.clone(), // Pass the explorer URL if available
            };

             // Call the send_message function
            if let Err(e) = send_message_to_telegram::send_message(&message).await {
                println!("Error sending message: {}", e);
            }
        }

    });

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
    #[derive(Debug)]
    pub struct TransferEvent {
        pub from: Address,
        pub to: Address,
        pub value: f64,
        pub tx_hash: H256,
        pub explorer: Option<String>,
    }

    // Remove the custom Transfer struct, as it's not needed.

    async fn get_provider(url: &str) -> Provider<Ws> {
        let ws = Ws::connect(url).await.expect("Failed to connect to WebSocket");
        Provider::new(ws)
    }

    pub async fn get_transfer_events(rpc: &str, contract_address: &str, decimal: u8, explorer: Option<String>, tx: mpsc::Sender<TransferEvent>) -> Result<(), Box<dyn std::error::Error>> {
        let provider = get_provider(rpc).await;
        let client = Arc::new(provider);
        let filter = Filter::new()
            .address(contract_address.parse::<Address>().unwrap())
            .event("Transfer(address,address,uint256)");

        let mut stream = client.subscribe_logs(&filter)
            .await
            .unwrap();
        while let Some(log) = stream.next().await {
            // topics[0] is the event signature, topics[1] is from, topics[2] is to
            let from: Address = Address::from(log.topics[1]);
            let to: Address = Address::from(log.topics[2]);
            let value: U256 = U256::decode(log.data.as_ref()).unwrap();
            let tx_hash: H256 = log.transaction_hash.unwrap_or_default();

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

    #[derive(Debug, Deserialize, Serialize)]
    pub struct Config {
        pub name: String,
        pub url: String,
        pub address: String,
        pub decimal: u8,
        pub explorer: Option<String>, // Optional field for explorer URL
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

    #[derive(Debug)]
    pub struct Message {
        pub from: Address,
        pub to: Address,
        pub value: f64,
        pub tx_hash: H256,
        // Optional field for explorer URL
        pub explorer: Option<String>,
    }


    pub async fn send_message(message: &Message) -> Result<(), Box<dyn std::error::Error>> {
        from_path("src/asset/.env").ok();

        if std::env::var("TELEGRAM_BOT_KEY").is_err() {
            return Err("TELEGRAM_BOT_KEY is not set".into());
        }

        let explorer_url = message.explorer.clone().unwrap_or_default();
        let from_link = format!(
            r#"<a href="{}/address/{:#x}">{:#x}</a>"#, 
            explorer_url, message.from, message.from
        );
        let to_link = format!(
            r#"<a href="{}/address/{:#x}">{:#x}</a>"#, 
            explorer_url, message.to, message.to
        );
        let tx_link = format!(
            r#"<a href="{}/tx/{:#x}">{:#x}</a>"#, 
            explorer_url, message.tx_hash, message.tx_hash
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