mod paper_trading;

use shared::*;
use zeromq::{SubSocket, PubSocket, Socket, SocketRecv, SocketSend};
use uuid::Uuid;
use rand::Rng;
use std::sync::Arc;
use tokio::sync::RwLock;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let mut order_subscriber = SubSocket::new();
    order_subscriber.connect("tcp://localhost:5559").await?;
    order_subscriber.subscribe("").await?;

    let mut book_subscriber = SubSocket::new();
    book_subscriber.connect("tcp://localhost:5556").await?;
    book_subscriber.subscribe("").await?;

    let mut report_publisher = PubSocket::new();
    report_publisher.bind("tcp://0.0.0.0:5560").await?;
    let report_publisher = Arc::new(tokio::sync::Mutex::new(report_publisher));

    // Paper trading engine with realistic execution simulation
    let paper_trading_engine = Arc::new(RwLock::new(
        paper_trading::PaperTradingEngine::new(2.5, 1.0) // 2.5 bps slippage, 1.0 bps market impact
    ));

    // Spawn task to update order books from market data
    let engine_clone = paper_trading_engine.clone();
    tokio::spawn(async move {
        loop {
            if let Ok(msg) = book_subscriber.recv().await {
                let msg_bytes: Vec<u8> = msg.try_into().unwrap_or_default();
                if let Ok(book_snapshot) = serde_json::from_slice::<OrderBookSnapshot>(&msg_bytes) {
                    let mut engine = engine_clone.write().await;
                    engine.update_market_data(&book_snapshot);
                }
            }
        }
    });

    println!("🎯 Paper Trading Engine started");
    println!("   Listening: OMS (orders) + Market Data (order books)");
    println!("   Publishing: Execution Reports → tcp://0.0.0.0:5560");
    println!("   Features: Realistic slippage, market impact, position tracking");

    loop {
        let msg = order_subscriber.recv().await?;
        let msg_bytes: Vec<u8> = msg.try_into()?;
        let order: OrderRequest = serde_json::from_slice(&msg_bytes)?;

        println!("🔄 Executing: {} ({} {})", order.order_id, 
            match order.side {
                OrderSide::Buy => "BUY",
                OrderSide::Sell => "SELL",
            },
            order.quantity
        );

        let engine_clone = paper_trading_engine.clone();
        let publisher_clone = report_publisher.clone();

        tokio::spawn(async move {
            if let Err(e) = process_order(order, engine_clone, publisher_clone).await {
                eprintln!("Error processing order: {}", e);
            }
        });
    }
}

async fn process_order(
    order: OrderRequest,
    engine: Arc<RwLock<paper_trading::PaperTradingEngine>>,
    publisher: Arc<tokio::sync::Mutex<PubSocket>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Simulate network latency (50-500 microseconds)
    let latency_us = rand::thread_rng().gen_range(50..500);
    tokio::time::sleep(tokio::time::Duration::from_micros(latency_us)).await;

    // Acknowledgement
    let ack = ExecutionReport {
        order_id: order.order_id.clone(),
        exec_id: Uuid::new_v4().to_string(),
        status: OrderStatus::Acknowledged,
        filled_qty: 0.0,
        fill_price: None,
        timestamp: order.timestamp,
    };

    let msg = serde_json::to_string(&ack)?;
    publisher.lock().await.send(msg.into()).await?;

    // Execute order using paper trading engine
    let report = {
        let mut engine = engine.write().await;
        engine.execute_order(&order)
    }; // Drop lock here before sending

    // Send execution report
    let msg = serde_json::to_string(&report)?;
    publisher.lock().await.send(msg.into()).await?;

    Ok(())
}

// Old MatchingEngine code removed - now using PaperTradingEngine from paper_trading module
