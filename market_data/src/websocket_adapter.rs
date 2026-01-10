// WebSocket Adapter for Live BTC Market Data
// Supports multiple exchanges via adapter pattern

use tokio_tungstenite::{connect_async, tungstenite::Message};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};
use shared::*;

#[derive(Debug, Clone, PartialEq)]
pub enum Exchange {
    CoinbasePro,
    Binance,
    // Add more exchanges as needed
}

#[derive(Clone)]
pub struct WebSocketAdapter {
    exchange: Exchange,
    symbol: String,
}

impl WebSocketAdapter {
    pub fn new(exchange: Exchange, symbol: String) -> Self {
        Self { exchange, symbol }
    }

    /// Get WebSocket URL for the exchange
    fn get_ws_url(&self) -> String {
        match self.exchange {
            Exchange::CoinbasePro => {
                // Note: Coinbase Pro was deprecated. Using Coinbase public feed as fallback
                // For production, use Coinbase Advanced Trade API with authentication
                // For now, we'll use a simpler public endpoint or suggest Binance
                format!("wss://ws-feed.exchange.coinbase.com")
            }
            Exchange::Binance => {
                // Binance public WebSocket stream - use depth stream for order book data
                // @depth20@100ms provides 20 levels of depth updated every 100ms
                let symbol_lower = self.symbol.to_lowercase().replace("-", "");
                format!("wss://stream.binance.com:9443/ws/{}@depth20@100ms", symbol_lower)
            }
        }
    }

    /// Create subscription message for the exchange
    fn create_subscription(&self) -> Value {
        match self.exchange {
            Exchange::CoinbasePro => {
                serde_json::json!({
                    "type": "subscribe",
                    "product_ids": [self.symbol],
                    "channels": ["level2", "ticker", "matches"]
                })
            }
            Exchange::Binance => {
                // Binance uses URL params for subscription, not JSON message
                serde_json::json!({})
            }
        }
    }

    /// Parse message from exchange into TickEvent(s)
    /// Returns a vector because Binance depth updates can contain multiple bid/ask updates
    pub fn parse_message(&self, msg: &str) -> Option<Vec<TickEvent>> {
        let json: Value = serde_json::from_str(msg).ok()?;
        
        match self.exchange {
            Exchange::CoinbasePro => {
                // Coinbase returns single tick events
                self.parse_coinbase_pro(&json).map(|tick| vec![tick])
            },
            Exchange::Binance => self.parse_binance(&json),
        }
    }

    fn parse_coinbase_pro(&self, json: &Value) -> Option<TickEvent> {
        let msg_type = json.get("type")?.as_str()?;
        
        match msg_type {
            "l2update" => {
                // Level 2 update
                let changes = json.get("changes")?.as_array()?;
                if let Some(change) = changes.first() {
                    let change_arr = change.as_array()?;
                    if change_arr.len() >= 3 {
                        let side_str = change_arr[0].as_str()?;
                        let price: f64 = change_arr[1].as_str()?.parse().ok()?;
                        let quantity: f64 = change_arr[2].as_str()?.parse().ok()?;
                        
                        let side = match side_str {
                            "buy" => Side::Bid,
                            "sell" => Side::Ask,
                            _ => return None,
                        };

                        let timestamp = json.get("time")?.as_str()
                            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                            .map(|dt| dt.timestamp_nanos_opt().unwrap_or(0))
                            .unwrap_or_else(|| now_ns() as i64);

                        return Some(TickEvent {
                            symbol: self.symbol.clone(),
                            timestamp_exchange: timestamp,
                            timestamp_recv: now_ns() as i64,
                            side,
                            price,
                            quantity,
                            sequence: 0, // Coinbase Pro doesn't provide sequence in l2update
                        });
                    }
                }
            }
            "match" => {
                // Trade
                let price: f64 = json.get("price")?.as_str()?.parse().ok()?;
                let quantity: f64 = json.get("size")?.as_str()?.parse().ok()?;
                let timestamp = json.get("time")?.as_str()
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.timestamp_nanos_opt().unwrap_or(0))
                    .unwrap_or_else(|| now_ns() as i64);

                return Some(TickEvent {
                    symbol: self.symbol.clone(),
                    timestamp_exchange: timestamp,
                    timestamp_recv: now_ns() as i64,
                    side: Side::Trade,
                    price,
                    quantity,
                    sequence: 0,
                });
            }
            _ => {}
        }
        
        None
    }

    fn parse_binance(&self, json: &Value) -> Option<Vec<TickEvent>> {
        // Binance depth stream format (@depth20@100ms)
        let event_type = json.get("e")?.as_str()?;
        
        if event_type == "depthUpdate" {
            // Parse depth update - extract bids and asks
            let bids = json.get("b")?.as_array()?;
            let asks = json.get("a")?.as_array()?;
            let timestamp = json.get("E")?.as_i64()? * 1_000_000; // Convert ms to ns
            let symbol = json.get("s")?.as_str()?.to_string();
            
            let mut ticks = Vec::new();
            
            // Process bids (bids are ordered by price descending)
            for bid in bids {
                if let Some(bid_arr) = bid.as_array() {
                    if bid_arr.len() >= 2 {
                        let price_str = bid_arr[0].as_str()?;
                        let qty_str = bid_arr[1].as_str()?;
                        let price: f64 = price_str.parse().ok()?;
                        let quantity: f64 = qty_str.parse().ok()?;
                        
                        if quantity > 0.0 {
                            ticks.push(TickEvent {
                                symbol: symbol.clone(),
                                timestamp_exchange: timestamp,
                                timestamp_recv: now_ns() as i64,
                                side: Side::Bid,
                                price,
                                quantity,
                                sequence: 0,
                            });
                        }
                    }
                }
            }
            
            // Process asks (asks are ordered by price ascending)
            for ask in asks {
                if let Some(ask_arr) = ask.as_array() {
                    if ask_arr.len() >= 2 {
                        let price_str = ask_arr[0].as_str()?;
                        let qty_str = ask_arr[1].as_str()?;
                        let price: f64 = price_str.parse().ok()?;
                        let quantity: f64 = qty_str.parse().ok()?;
                        
                        if quantity > 0.0 {
                            ticks.push(TickEvent {
                                symbol: symbol.clone(),
                                timestamp_exchange: timestamp,
                                timestamp_recv: now_ns() as i64,
                                side: Side::Ask,
                                price,
                                quantity,
                                sequence: 0,
                            });
                        }
                    }
                }
            }
            
            if !ticks.is_empty() {
                return Some(ticks);
            }
        } else if event_type == "24hrTicker" {
            // Fallback: ticker format (trade only)
            let price: f64 = json.get("c")?.as_str()?.parse().ok()?;
            let quantity: f64 = json.get("v")?.as_str()?.parse().ok()?;
            let timestamp = json.get("E")?.as_i64()? * 1_000_000;

            return Some(vec![TickEvent {
                symbol: self.symbol.clone(),
                timestamp_exchange: timestamp,
                timestamp_recv: now_ns() as i64,
                side: Side::Trade,
                price,
                quantity,
                sequence: 0,
            }]);
        }
        
        None
    }
}

pub async fn connect_and_stream(
    adapter: WebSocketAdapter,
    tx: tokio::sync::mpsc::UnboundedSender<TickEvent>,
) -> Result<(), Box<dyn std::error::Error>> {
    let url_str = adapter.get_ws_url();
    println!("🔗 Attempting to connect to: {}", url_str);
    
    let url = url::Url::parse(&url_str)?;
    let (ws_stream, response) = match connect_async(url.clone()).await {
        Ok((stream, resp)) => (stream, resp),
        Err(e) => {
            eprintln!("❌ Failed to connect to {}: {}", url_str, e);
            eprintln!("💡 Tip: Coinbase Pro was deprecated. Consider using Binance or Coinbase Advanced Trade API");
            return Err(e.into());
        }
    };
    
    // Log connection details
    let status = response.status();
    if !status.is_success() {
        eprintln!("⚠️  WebSocket handshake returned status: {}", status);
    }
    
    let (mut write, mut read) = ws_stream.split();

    // Send subscription message (if needed)
    if adapter.exchange == Exchange::CoinbasePro {
        let sub_msg = adapter.create_subscription();
        if let Err(e) = write.send(Message::Text(sub_msg.to_string())).await {
            eprintln!("⚠️  Failed to send subscription message: {}", e);
            return Err(e.into());
        }
    }

    println!("✅ Connected to {} WebSocket", match adapter.exchange {
        Exchange::CoinbasePro => "Coinbase",
        Exchange::Binance => "Binance",
    });

    while let Some(msg) = read.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                if let Some(ticks) = adapter.parse_message(&text) {
                    for tick in ticks {
                        if tx.send(tick).is_err() {
                            eprintln!("⚠️  Channel closed, stopping WebSocket stream");
                            break;
                        }
                    }
                }
            }
            Ok(Message::Close(_)) => {
                println!("⚠️  WebSocket connection closed");
                break;
            }
            Err(e) => {
                eprintln!("⚠️  WebSocket error: {}", e);
                break;
            }
            _ => {}
        }
    }

    Ok(())
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

