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