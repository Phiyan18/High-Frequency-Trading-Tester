#![allow(special_module_name)]

mod lib;
mod mdh;
mod websocket_adapter;

use zeromq::{PubSocket, Socket, SocketSend};
use std::time::SystemTime;
use tokio::sync::mpsc;
use shared::Side;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let mut tick_publisher = PubSocket::new();
    tick_publisher.bind("tcp://0.0.0.0:5555").await?;

    let mut book_publisher = PubSocket::new();
    book_publisher.bind("tcp://0.0.0.0:5556").await?;

    let mut candle_publisher = PubSocket::new();
    candle_publisher.bind("tcp://0.0.0.0:5561").await?;

    println!("📊 Market Data Handler started (Live WebSocket Mode)");
    println!("   Ticks → tcp://0.0.0.0:5555");
    println!("   Books → tcp://0.0.0.0:5556");
    println!("   Candles → tcp://0.0.0.0:5561");
    println!("   Data Source: Binance WebSocket (Live BTC-USDT)");

    // Run WebSocket mode for live data
    run_websocket_mode(tick_publisher, book_publisher, candle_publisher).await?;

    Ok(())
}

async fn run_websocket_mode(
    mut tick_publisher: PubSocket,
    mut book_publisher: PubSocket,
    _candle_publisher: PubSocket,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("🌐 Connecting to WebSocket for live BTC data...");
    
    let (tx, mut rx) = mpsc::unbounded_channel();
    
    // Note: Coinbase Pro was deprecated. Using Binance as default (more reliable public API)
    // To use Coinbase, you need Coinbase Advanced Trade API with authentication
    let use_binance = std::env::var("USE_BINANCE").unwrap_or_else(|_| "true".to_string()) == "true";
    
    let adapter = if use_binance {
        println!("   Using Binance WebSocket (BTC-USDT)");
        websocket_adapter::WebSocketAdapter::new(
            websocket_adapter::Exchange::Binance,
            "BTC-USDT".to_string(),
        )
    } else {
        println!("   Using Coinbase (may fail - Coinbase Pro was deprecated)");
        websocket_adapter::WebSocketAdapter::new(
            websocket_adapter::Exchange::CoinbasePro,
            "BTC-USD".to_string(),
        )
    };

    // Spawn WebSocket connection task
    let adapter_clone = adapter.clone();
    tokio::spawn(async move {
        if let Err(e) = websocket_adapter::connect_and_stream(adapter_clone, tx).await {
            eprintln!("❌ WebSocket connection error: {}", e);
        }
    });

    // Enhanced MDH for processing ticks
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let mdh = mdh::MarketDataHandler::new(event_tx, 10000);
    let mdh_arc = std::sync::Arc::new(tokio::sync::RwLock::new(mdh));

    let mut book_builder = lib::OrderBookBuilder::new();
    let mut last_book_publish = SystemTime::now();
    let mut last_candle_publish = SystemTime::now();

    println!("✅ WebSocket mode active, processing live market data...");

    let mut tick_count = 0u64;
    let mut depth_update_count = 0u64;
    
    while let Some(tick) = rx.recv().await {
        tick_count += 1;
        if tick_count == 1 {
            println!("✅ First tick received! side={:?} price={:.2} qty={:.4}", tick.side, tick.price, tick.quantity);
        }
        
        // Count depth updates (bid/ask, not trades)
        if matches!(tick.side, Side::Bid | Side::Ask) {
            depth_update_count += 1;
            if depth_update_count % 20 == 0 {
                println!("📊 Processed {} depth updates (bids/asks) | {}: {:?} price={:.2}", 
                    depth_update_count, tick.symbol, tick.side, tick.price);
            }
        }

        // Process tick through MDH
        {
            let mdh_guard = mdh_arc.read().await;
            if let Err(e) = mdh_guard.process_tick(tick.clone()).await {
                eprintln!("⚠️  Error processing tick: {}", e);
            }
        }

        // Update order book builder
        book_builder.update(&tick);

        // Publish tick
        if let Ok(msg) = serde_json::to_string(&tick) {
            let _ = tick_publisher.send(msg.into()).await;
            tick_count += 1;
        }

        // Publish order book snapshot periodically (every 100ms)
        let now = SystemTime::now();
        if now.duration_since(last_book_publish).unwrap().as_millis() >= 100 {
            // Update book builder (we don't need to use simple_book, just ensure it's updated)
            let _ = book_builder.get_snapshot(&tick.symbol);
            // Get enhanced snapshot from MDH
            let mdh_guard = mdh_arc.read().await;
            if let Some(enhanced_book) = mdh_guard.get_snapshot(&tick.symbol, 20).await {
                    // Verify book has data before publishing
                    if !enhanced_book.bids.is_empty() && !enhanced_book.asks.is_empty() && enhanced_book.mid_price > 0.0 {
                        if let Ok(msg) = serde_json::to_string(&enhanced_book) {
                            let _ = book_publisher.send(msg.into()).await;
                            // Debug: log first few books
                            if depth_update_count <= 20 {
                                println!("📚 Published order book: {} | mid={:.2} | bids={} | asks={} | spread={:.4}", 
                                    enhanced_book.symbol, enhanced_book.mid_price, 
                                    enhanced_book.bids.len(), enhanced_book.asks.len(), enhanced_book.spread);
                            }
                        }
                    } else {
                        // Debug: log why book wasn't published
                        if depth_update_count <= 5 {
                            println!("⚠️  Order book not ready: bids={} asks={} mid_price={:.2}", 
                                enhanced_book.bids.len(), enhanced_book.asks.len(), enhanced_book.mid_price);
                        }
                    }
            }
            last_book_publish = now;
        }

        // Publish candlestick data periodically (every 1 second)
        // Note: For live data, candles would need to be aggregated from ticks
        // For now, we can skip candle publishing or implement tick aggregation
        if now.duration_since(last_candle_publish).unwrap().as_secs() >= 1 {
            // Candlestick aggregation from ticks would go here
            // For now, we'll skip this as it requires tick aggregation logic
            last_candle_publish = now;
        }

        // Log progress every 1000 ticks
        if tick_count % 1000 == 0 {
            println!("📊 Processed {} ticks from live feed...", tick_count);
        }
    }

    Ok(())
}
