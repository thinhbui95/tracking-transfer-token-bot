use teloxide::{ Bot, requests::Requester, payloads::SendMessageSetters };
use dotenv::from_path;
use once_cell::sync::OnceCell;
use crate::types::{ MessageToSend, TransferType };

/// Global singleton Bot instance, initialized once on first use
static TELEGRAM_BOT: OnceCell<Bot> = OnceCell::new();
/// Global singleton Chat ID, initialized once on first use
static CHAT_ID: OnceCell<i64> = OnceCell::new();

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

/// Get or initialize the chat ID (singleton pattern)
fn get_chat_id() -> Result<i64, Box<dyn std::error::Error + Send + Sync>> {
    CHAT_ID.get_or_try_init(|| {
        from_path("src/asset/.env").ok();
        let chat_id: i64 = std::env::var("TELEGRAM_CHAT_ID")
            .map_err(|_| "TELEGRAM_CHAT_ID not set")?.parse()
            .map_err(|_| "Invalid TELEGRAM_CHAT_ID")?;
        Ok(chat_id)
    }).map(|id| *id)
}

/// Macro to create HTML links for blockchain explorers
macro_rules! link {
    ($url:expr, $path:expr, $text:expr) => {
        format!(r#"<a href="{}/{}">{}</a>"#, $url, $path, $text)
    };
}

/// Sends a formatted message to a Telegram chat using the bot.
/// The message includes clickable links for the from/to addresses and transaction hash.
/// Supports both Ethereum and Solana transfers.
pub async fn send_message(message: &MessageToSend) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let chat_id = get_chat_id()?;
    let bot = get_telegram_bot()?;
    
    // Closure to format transfer message
    let format_message = |emoji: &str, chain: &str, from_link: String, to_link: String, amount: f64, tx_link: String| {
        format!(
            "{} <b>{} Transfer</b>\n\nFrom: {}\nTo: {}\n💰 Amount: {}\n📝 Tx: {}",
            emoji, chain, from_link, to_link, amount, tx_link
        )
    };
    
    let text = match &message.transfer {
        TransferType::EVM { from, to, value, tx_hash } => {
            let explorer = message.explorer.as_deref().unwrap_or_default();
            let from_link = link!(explorer, format!("address/{:#x}", from), from.to_string());
            let to_link = link!(explorer, format!("address/{:#x}", to), to.to_string());
            let tx_link = link!(explorer, format!("tx/{:#x}", tx_hash), "Detail");
            
            format_message("🔷", "EVM", from_link, to_link, *value, tx_link)
        },
        TransferType::Solana { from, to, amount, tx_signature } => {
            let explorer = message.explorer.as_deref().unwrap_or("https://solscan.io");
            let from_link = link!(explorer, format!("account/{}", from), format!("{}...", &from[..8]));
            let to_link = link!(explorer, format!("account/{}", to), format!("{}...", &to[..8]));
            let tx_link = link!(explorer, format!("tx/{}", tx_signature), "Detail");
            
            format_message("🟣", "Solana", from_link, to_link, *amount, tx_link)
        }
    };

    bot.send_message(teloxide::types::ChatId(chat_id), text)
        .parse_mode(teloxide::types::ParseMode::Html)
        .await
        .map_err(|e| format!("Failed to send message: {}", e))?;
    
    Ok(())
}
