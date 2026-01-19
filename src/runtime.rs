use crate::strategy::Strategy;
use crate::types::*;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use zeromq::{Socket, SocketRecv, SocketSend, SubSocket, PubSocket};

pub struct StrategyRuntime {
    strategies: Arc<RwLock<HashMap<String, Box<dyn Strategy>>>>,
    signal_sender: mpsc::UnboundedSender<Signal>,
    metrics: Arc<RwLock<HashMap<String, StrategyMetrics>>>,
}

impl StrategyRuntime {
    pub fn new(signal_sender: mpsc::UnboundedSender<Signal>) -> Self {
        Self {
            strategies: Arc::new(RwLock::new(HashMap::new())),
            signal_sender,
            metrics: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn register_strategy(&self, strategy: Box<dyn Strategy>) {
        let name = strategy.name().to_string();
        
        let mut strategies = self.strategies.write().await;
        strategies.insert(name.clone(), strategy);
        
        let mut metrics = self.metrics.write().await;
        metrics.insert(name, StrategyMetrics {
            signals_generated: 0,
            execution_time_us: Vec::new(),
            errors: 0,
        });
    }

    pub async fn run(&self, market_data_endpoint: &str) -> Result<(), Box<dyn std::error::Error>> {
        // Subscribe to order book updates
        let mut book_subscriber = SubSocket::new();
        book_subscriber.connect(market_data_endpoint).await?;
        book_subscriber.subscribe("").await?;

        // Subscribe to candlestick updates
        let mut candle_subscriber = SubSocket::new();
        candle_subscriber.connect("tcp://localhost:5561").await?;
        candle_subscriber.subscribe("").await?;

        println!("Strategy Runtime listening on {} (books) and tcp://localhost:5561 (candles)", market_data_endpoint);

        let strategies = Arc::clone(&self.strategies);
        let signal_sender = self.signal_sender.clone();
        let metrics = Arc::clone(&self.metrics);
        
        // Spawn task for order books
        let strategies_books = Arc::clone(&strategies);
        let signal_sender_books = signal_sender.clone();
        let metrics_books = Arc::clone(&metrics);
        tokio::spawn(async move {
            loop {
                if let Ok(msg) = book_subscriber.recv().await {
                    let msg_bytes: Vec<u8> = msg.try_into().unwrap_or_default();
                    if let Ok(book) = serde_json::from_slice::<OrderBookSnapshot>(&msg_bytes) {
                        let strategies = strategies_books.read().await;
                        for (name, strategy_arc) in strategies.iter() {
                            let start = std::time::Instant::now();
                            let signals = match strategy_arc.on_orderbook(&book).await {
                                Ok(s) => s,
                                Err(e) => {
                                    eprintln!("Strategy {} error: {}", name, e);
                                    let mut metrics = metrics_books.write().await;
                                    if let Some(m) = metrics.get_mut(name) {
                                        m.errors += 1;
                                    }
                                    continue;
                                }
                            };
                            let elapsed = start.elapsed().as_micros() as u64;
                            let mut metrics = metrics_books.write().await;
                            if let Some(m) = metrics.get_mut(name) {
                                m.execution_time_us.push(elapsed);
                                m.signals_generated += signals.len() as u64;
                                if m.execution_time_us.len() > 1000 {
                                    m.execution_time_us.remove(0);
                                }
                            }
                            for signal in signals {
                                if let Err(e) = signal_sender_books.send(signal) {
                                    eprintln!("Failed to send signal: {}", e);
                                }
                            }
                        }
                    }
                }
            }
        });

        // Spawn task for candlesticks
        let strategies_candles = Arc::clone(&strategies);
        let signal_sender_candles = signal_sender.clone();
        let metrics_candles = Arc::clone(&metrics);
        tokio::spawn(async move {
            loop {
                if let Ok(msg) = candle_subscriber.recv().await {
                    let msg_bytes: Vec<u8> = msg.try_into().unwrap_or_default();
                    if let Ok(candle) = serde_json::from_slice::<shared::CandleStick>(&msg_bytes) {
                        let strategies = strategies_candles.read().await;
                        for (name, strategy_arc) in strategies.iter() {
                            let start = std::time::Instant::now();
                            let signals = match strategy_arc.on_candlestick(&candle).await {
                                Ok(s) => s,
                                Err(e) => {
                                    eprintln!("Strategy {} error on candlestick: {}", name, e);
                                    let mut metrics = metrics_candles.write().await;
                                    if let Some(m) = metrics.get_mut(name) {
                                        m.errors += 1;
                                    }
                                    continue;
                                }
                            };
                            let elapsed = start.elapsed().as_micros() as u64;
                            let mut metrics = metrics_candles.write().await;
                            if let Some(m) = metrics.get_mut(name) {
                                m.execution_time_us.push(elapsed);
                                m.signals_generated += signals.len() as u64;
                                if m.execution_time_us.len() > 1000 {
                                    m.execution_time_us.remove(0);
                                }
                            }
                            for signal in signals {
                                if let Err(e) = signal_sender_candles.send(signal) {
                                    eprintln!("Failed to send signal: {}", e);
                                }
                            }
                        }
                    }
                }
            }
        });

        // Keep main task alive
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }
    }

    async fn process_orderbook(&self, book: OrderBookSnapshot) {
        let strategies = self.strategies.read().await;
        
        for (name, strategy_arc) in strategies.iter() {
            let start = std::time::Instant::now();
            
            // Execute strategy (need interior mutability here)
            // In production, use Arc<Mutex<>> or channels for strategy access
            let signals = match strategy_arc.on_orderbook(&book).await {
                Ok(signals) => signals,
                Err(e) => {
                    eprintln!("Strategy {} error: {}", name, e);
                    let mut metrics = self.metrics.write().await;
                    if let Some(m) = metrics.get_mut(name) {
                        m.errors += 1;
                    }
                    continue;
                }
            };

            let elapsed = start.elapsed().as_micros() as u64;

            // Update metrics
            let mut metrics = self.metrics.write().await;
            if let Some(m) = metrics.get_mut(name) {
                m.execution_time_us.push(elapsed);
                m.signals_generated += signals.len() as u64;
                
                // Keep only last 1000 timing samples
                if m.execution_time_us.len() > 1000 {
                    m.execution_time_us.remove(0);
                }
            }

            // Send signals downstream
            for signal in signals {
                if let Err(e) = self.signal_sender.send(signal) {
                    eprintln!("Failed to send signal: {}", e);
                }
            }
        }
    }


    pub async fn get_metrics(&self) -> HashMap<String, StrategyMetrics> {
        self.metrics.read().await.clone()
    }

    pub async fn shutdown(&self) {
        let mut strategies = self.strategies.write().await;
        for (name, strategy) in strategies.iter_mut() {
            if let Err(e) = strategy.shutdown() {
                eprintln!("Error shutting down {}: {}", name, e);
            }
        }
    }
}

// Note: StrategyRuntime doesn't need Clone - we'll use Arc instead