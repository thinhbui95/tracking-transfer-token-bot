# tracking-transfer-token-bot

A Rust-based service for monitoring ERC-20 token Transfer events on multiple blockchains and sending formatted notifications to a Telegram chat.

## Features

- Listens for ERC-20 `Transfer` events on multiple EVM-compatible blockchains (Ethereum, BSC, Viction, etc.)
- Uses WebSocket RPC endpoints for real-time event streaming
- Sends detailed, clickable notifications to a Telegram chat, including:
  - From/To addresses (with explorer links)
  - Value (human-readable)
  - Transaction hash (with explorer link)
- Configurable via `src/asset/config.json`
- Securely loads secrets from `src/asset/.env`

## Configuration

### 1. Blockchain Config

Edit `src/asset/config.json` to add or update networks:

```json
{
  "BSC Mainnet": {
    "name": "BSC Mainnet",
    "url": "wss://bsc-rpc.publicnode.com",
    "address": "0xaec945e04baf28b135fa7c640f624f8d90f1c3a6",
    "decimal": 18,
    "explorer": "https://bscscan.com"
  },
  "ETHEREUM": {
    "name": "ETHEREUM",
    "url": "wss://mainnet.infura.io/ws/v3/YOUR_INFURA_KEY",
    "address": "0xdac17f958d2ee523a2206206994597c13d831ec7",
    "decimal": 6,
    "explorer": "https://etherscan.io"
  },
   "VICTION": {
    "name": "VICTION",
    "url": "wss://viction.drpc.org",
    "address": "0x0Fd0288AAAE91eaF935e2eC14b23486f86516c8C",
    "decimal": 18,
    "explorer": "https://vicscan.xyz"
  }
}
```

### 2. Telegram Bot & Chat

Create `src/asset/.env`:

```
TELEGRAM_BOT_KEY=your_bot_token
TELEGRAM_CHAT_ID=your_chat_id
```

- [How to get a bot token](https://core.telegram.org/bots#6-botfather)
- [How to get a chat ID](https://stackoverflow.com/a/32572159)

## Usage

1. Install Rust (https://rustup.rs)
2. Install dependencies:
   ```sh
   cargo build
   ```
3. Run the service:
   ```sh
   cargo run
   ```

## How it works

- For each configured network, a background task subscribes to Transfer events.
- When an event is detected, it is sent through a channel to a consumer task.
- The consumer formats the event and sends a message to the configured Telegram chat.

## Example Telegram Message

```
Transfer Event:
From: <a href="https://etherscan.io/address/0x...">0x...</a>
To: <a href="https://etherscan.io/address/0x...">0x...</a>
Value: 123.45
Transaction Hash: <a href="https://etherscan.io/tx/0x...">0x...</a>
```
