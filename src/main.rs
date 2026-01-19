mod types;
mod strategy;
mod strategies;
mod runtime;
mod signal_converter;

use runtime::StrategyRuntime;
use strategies::market_maker::SimpleMarketMaker;
use strategies::mean_reversion::MeanReversion;
use strategies::candlestick_ema::CandlestickEmaStrategy;
use types::*;
use std::collections::HashMap;
use tokio::sync::mpsc;
use zeromq::{PubSocket, SocketSend};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    // Create signal channel
    let (signal_tx, mut signal_rx) = mpsc::unbounded_channel::<Signal>();

    // Create runtime
    let runtime = StrategyRuntime::new(signal_tx);

    // Register strategies - Aggressive HFT Market Maker
    let mut mm_strategy = Box::new(SimpleMarketMaker::new("MarketMaker-1".to_string()));
    let mut params = HashMap::new();
    params.insert("spread_bps".to_string(), "8".to_string());  // 8 bps spread for better fills
    params.insert("quantity".to_string(), "0.5".to_string());  // Smaller size for more trades
    params.insert("max_inventory".to_string(), "5.0".to_string());
    params.insert("ema_period".to_string(), "20".to_string());  // Faster EMA
    params.insert("min_signal_interval_ms".to_string(), "50".to_string());  // More frequent
    params.insert("depth_levels".to_string(), "5".to_string());  // Use top 5 levels
    mm_strategy.initialize(params)?;
    runtime.register_strategy(mm_strategy).await;

    let mut mr_strategy = Box::new(MeanReversion::new("MeanRev-1".to_string(), 100));
    let mut params = HashMap::new();
    params.insert("entry_threshold".to_string(), "2.5".to_string());
    mr_strategy.initialize(params)?;
    runtime.register_strategy(mr_strategy).await;

    // Register candlestick-based EMA strategy
    let mut ema_strategy = Box::new(CandlestickEmaStrategy::new("CandlestickEMA-1".to_string(), 12, 26));
    let mut params = HashMap::new();
    params.insert("fast_period".to_string(), "12".to_string());
    params.insert("slow_period".to_string(), "26".to_string());
    params.insert("quantity".to_string(), "1.0".to_string());
    ema_strategy.initialize(params)?;
    runtime.register_strategy(ema_strategy).await;

    // Setup signal publisher (to Risk Management)
    // Note: Risk management listens to signals on port 5557, not orders
    let mut signal_publisher = PubSocket::new();
    signal_publisher.bind("tcp://0.0.0.0:5557").await?;

    // Spawn runtime - connect to order book updates on port 5556
    let runtime_clone = runtime.clone();
    tokio::spawn(async move {
        runtime_clone.run("tcp://localhost:5556").await.unwrap();
    });

    // Spawn signal processor
    tokio::spawn(async move {
        let mut pub_socket = signal_publisher;
        let mut signal_count = 0u64;
        while let Some(signal) = signal_rx.recv().await {
            signal_count += 1;
            println!("📈 Signal #{}: {} | {} | {:?} | qty={:.4} @ {:?} | conf={:.2}",
                signal_count,
                signal.strategy_name,
                signal.symbol,
                signal.signal_type,
                signal.quantity,
                signal.price,
                signal.confidence
            );

            // Publish signal to risk management (they convert to orders)
            let msg = serde_json::to_string(&signal).unwrap();
            if let Err(e) = pub_socket.send(msg.into()).await {
                eprintln!("❌ Failed to publish signal: {}", e);
            }
        }
    });

    // Metrics reporter
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
        
        let metrics = runtime.get_metrics().await;
        println!("\n=== Strategy Metrics ===");
        for (name, m) in metrics {
            let avg_time = if !m.execution_time_us.is_empty() {
                m.execution_time_us.iter().sum::<u64>() / m.execution_time_us.len() as u64
            } else {
                0
            };
            
            println!("{}: signals={} | avg_exec={}μs | errors={}",
                name, m.signals_generated, avg_time, m.errors);
        }
    }
}