use teloxide::{ Bot, requests::Requester, payloads::SendMessageSetters };
use dotenv::from_path;
use ethers::core::types::Address;
use ethers::prelude::*;
use once_cell::sync::OnceCell;

/// Global singleton Bot instance, initialized once on first use
static TELEGRAM_BOT: OnceCell<Bot> = OnceCell::new();

/// Get or initialize the Telegram bot instance (singleton pattern)
pub fn get_telegram_bot() -> Result<&'static Bot, Box<dyn std::error::Error + Send + Sync>> {
    TELEGRAM_BOT.get_or_try_init(|| {
        // Load environment variables
        from_path("src/asset/.env").ok();
        
        let bot_token = std::env::var("TELEGRAM_BOT_KEY")
            .map_err(|_| "TELEGRAM_BOT_KEY not set")?;
        
        Ok::<Bot, Box<dyn std::error::Error + Send + Sync>>(Bot::new(bot_token))
    })
}

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
pub async fn send_message(message: &Message) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Load environment variables from the custom .env path
    from_path("src/asset/.env").ok();

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

    // Get the shared bot instance (created only once)
    let bot = get_telegram_bot()?;

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
