mod backtest;
mod tui_dashboard;

use shared::*;
use zeromq::{SubSocket, Socket, SocketRecv};
use prometheus::{Registry, Counter, Histogram, Gauge, TextEncoder, Encoder};
use axum::{
    Router, 
    routing::{get, post}, 
    response::{IntoResponse, Redirect},
    extract::{State, Query},
    Json,
    http::StatusCode,
};
use redis::{AsyncCommands, Client as RedisClient};
use std::sync::Arc;
use tokio::sync::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::{VecDeque, HashMap};
use std::time::{SystemTime, UNIX_EPOCH};
use std::env;
use chrono::NaiveDate;

#[derive(Clone)]
struct AppState {
    registry: Arc<Registry>,
    redis_client: Arc<RedisClient>,
    recent_trades: Arc<RwLock<VecDeque<TradeEvent>>>,
    recent_orders: Arc<RwLock<VecDeque<OrderEvent>>>,
    #[allow(dead_code)] // Used in spawned execution subscriber task
    orders_by_id: Arc<RwLock<HashMap<String, OrderRequest>>>,
    metrics: Arc<Metrics>,
    rate_tracker: Arc<RwLock<RateTracker>>,
    recent_candles: Arc<RwLock<VecDeque<CandleStick>>>,
    order_books: Arc<RwLock<HashMap<String, OrderBookSnapshot>>>,
}

pub(crate) struct RateTracker {
    tick_times: VecDeque<u64>,
    signal_times: VecDeque<u64>,
    order_times: VecDeque<u64>,
    tick_latencies: VecDeque<f64>,
    window_seconds: u64,
}

impl RateTracker {
    fn new(window_seconds: u64) -> Self {
        Self {
            tick_times: VecDeque::new(),
            signal_times: VecDeque::new(),
            order_times: VecDeque::new(),
            tick_latencies: VecDeque::new(),
            window_seconds,
        }
    }

    fn record_tick(&mut self, latency: f64) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        self.tick_times.push_back(now);
        self.tick_latencies.push_back(latency);
        
        // Keep only last window
        let cutoff = now.saturating_sub(self.window_seconds);
        while let Some(&time) = self.tick_times.front() {
            if time < cutoff {
                self.tick_times.pop_front();
                self.tick_latencies.pop_front();
            } else {
                break;
            }
        }
    }

    fn record_signal(&mut self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        self.signal_times.push_back(now);
        
        let cutoff = now.saturating_sub(self.window_seconds);
        while let Some(&time) = self.signal_times.front() {
            if time < cutoff {
                self.signal_times.pop_front();
            } else {
                break;
            }
        }
    }

    fn record_order(&mut self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        self.order_times.push_back(now);
        
        let cutoff = now.saturating_sub(self.window_seconds);
        while let Some(&time) = self.order_times.front() {
            if time < cutoff {
                self.order_times.pop_front();
            } else {
                break;
            }
        }
    }

    fn get_ticks_per_second(&self) -> f64 {
        self.tick_times.len() as f64 / self.window_seconds as f64
    }

    fn get_signals_per_second(&self) -> f64 {
        self.signal_times.len() as f64 / self.window_seconds as f64
    }

    fn get_orders_per_second(&self) -> f64 {
        self.order_times.len() as f64 / self.window_seconds as f64
    }

    fn get_latency_stats(&self) -> (f64, f64, f64) {
        if self.tick_latencies.is_empty() {
            return (0.0, 0.0, 0.0);
        }
        
        let sum: f64 = self.tick_latencies.iter().sum();
        let avg = sum / self.tick_latencies.len() as f64;
        let min = *self.tick_latencies.iter().min_by(|a, b| a.partial_cmp(b).unwrap()).unwrap_or(&0.0);
        let max = *self.tick_latencies.iter().max_by(|a, b| a.partial_cmp(b).unwrap()).unwrap_or(&0.0);
        
        (avg, min, max)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TradeEvent {
    order_id: String,
    symbol: String,
    side: String,
    quantity: f64,
    price: f64,
    status: String,
    timestamp: i64,
    strategy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct OrderEvent {
    order_id: String,
    symbol: String,
    side: String,
    quantity: f64,
    price: Option<f64>,
    status: String,
    timestamp: i64,
    strategy: String,
}

#[derive(Debug, Serialize)]
struct Position {
    symbol: String,
    quantity: f64,
    avg_price: Option<f64>,
}

#[derive(Debug, Serialize)]
struct RiskStatus {
    kill_switch_active: bool,
    daily_loss: f64,
    max_daily_loss: f64,
    max_position: f64,
    max_order_size: f64,
}

#[derive(Debug, Deserialize)]
struct KillSwitchRequest {
    active: bool,
}

// Helper function to connect subscriber with retries
async fn connect_subscriber(endpoint: &str, name: &str, retries: u32) -> Option<SubSocket> {
    for attempt in 1..=retries {
        let mut sub = SubSocket::new();
        match tokio::time::timeout(
            tokio::time::Duration::from_secs(2),
            sub.connect(endpoint)
        ).await {
            Ok(Ok(_)) => {
                tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                match tokio::time::timeout(
                    tokio::time::Duration::from_secs(1),
                    sub.subscribe("")
                ).await {
                    Ok(Ok(_)) => {
                        println!("✅ Connected to {} (attempt {})", name, attempt);
                        return Some(sub);
                    }
                    _ => {
                        if attempt < retries {
                            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                            continue;
                        }
                    }
                }
            }
            _ => {
                if attempt < retries {
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                    continue;
                }
            }
        }
    }
    // Don't print warning here - will be handled by background retry
    None
}

// Background retry function that continues trying to connect
async fn connect_subscriber_with_retry<F>(endpoint: &'static str, name: &'static str, callback: F)
where
    F: FnOnce(SubSocket) + Send + 'static,
{
    let mut attempt = 0;
    loop {
        attempt += 1;
        let mut sub = SubSocket::new();
        match tokio::time::timeout(
            tokio::time::Duration::from_secs(2),
            sub.connect(endpoint)
        ).await {
            Ok(Ok(_)) => {
                tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                match tokio::time::timeout(
                    tokio::time::Duration::from_secs(1),
                    sub.subscribe("")
                ).await {
                    Ok(Ok(_)) => {
                        println!("✅ Connected to {} (background retry, attempt {})", name, attempt);
                        callback(sub);
                        return; // Successfully connected, exit retry loop
                    }
                    _ => {
                        // Continue retrying
                    }
                }
            }
            _ => {
                // Continue retrying
            }
        }
        // Wait before next retry (exponential backoff, max 5 seconds)
        let delay = std::cmp::min(500 * attempt, 5000);
        tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    // Check for command-line arguments to choose dashboard mode
    let use_tui = env::args().any(|arg| arg == "--tui" || arg == "-t");

    // Connect to Redis - create client (this should always succeed, connection happens later)
    // Endpoints will handle connection errors gracefully  
    let redis_client = RedisClient::open("redis://127.0.0.1/")
        .unwrap_or_else(|e| {
            eprintln!("⚠️  Warning: Could not create Redis client: {}. Service will start but Redis features may not work.", e);
            // Try alternative URL format
            RedisClient::open("redis://127.0.0.1:6379/").expect("Failed to create Redis client - invalid URL format")
        });
    println!("✅ Redis client created");
    let redis_client = Arc::new(redis_client);

    // Create Prometheus registry
    let registry = Arc::new(Registry::new());
    println!("✅ Prometheus registry created");
    
    // Define metrics
    let metrics = Arc::new(Metrics::new(&registry));
    println!("✅ Metrics initialized");

    // Wait a bit for publishers to be ready
    println!("📡 Waiting for publishers to be ready...");
    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
    
    // Subscribe to all component streams with retry logic
    println!("📡 Connecting to ZeroMQ subscribers...");
    
    let tick_subscriber = connect_subscriber("tcp://localhost:5555", "market data stream", 5).await;

    let signal_subscriber = connect_subscriber("tcp://localhost:5557", "signals stream", 5).await;

    let order_subscriber = connect_subscriber("tcp://localhost:5558", "orders stream", 5).await;

    let exec_subscriber = connect_subscriber("tcp://localhost:5560", "executions stream", 5).await;

    let candle_subscriber = connect_subscriber("tcp://localhost:5561", "candlestick stream", 5).await;

    let book_subscriber = connect_subscriber("tcp://localhost:5556", "order book stream", 5).await;

    let recent_trades = Arc::new(RwLock::new(VecDeque::with_capacity(1000)));
    let recent_orders = Arc::new(RwLock::new(VecDeque::with_capacity(1000)));
    let orders_by_id = Arc::new(RwLock::new(HashMap::new()));
    let recent_candles = Arc::new(RwLock::new(VecDeque::with_capacity(500)));
    let order_books = Arc::new(RwLock::new(HashMap::new()));

    let rate_tracker = Arc::new(RwLock::new(RateTracker::new(1))); // 1 second window

    let app_state = AppState {
        registry: registry.clone(),
        redis_client: redis_client.clone(),
        recent_trades: recent_trades.clone(),
        recent_orders: recent_orders.clone(),
        orders_by_id: orders_by_id.clone(),
        metrics: metrics.clone(),
        rate_tracker: rate_tracker.clone(),
        recent_candles: recent_candles.clone(),
        order_books: order_books.clone(),
    };

    println!("📊 Monitoring System started");
    println!("   Dashboard: http://localhost:9090/dashboard");
    println!("   API: http://localhost:9090/api/*");
    println!("   Note: ZeroMQ connections will be established when publishers are available");

    // Spawn separate tasks for each subscriber
    if let Some(mut tick_sub) = tick_subscriber {
        let metrics_clone = metrics.clone();
        let rate_tracker_clone = rate_tracker.clone();
        tokio::spawn(async move {
            let mut tick_count = 0u64;
            println!("📊 Tick subscriber task started, waiting for messages...");
            loop {
                if let Ok(msg) = tick_sub.recv().await {
                    let msg_bytes: Vec<u8> = msg.try_into().unwrap_or_default();
                    if let Ok(tick) = serde_json::from_slice::<TickEvent>(&msg_bytes) {
                        tick_count += 1;
                        if tick_count % 1000 == 0 {
                            let latency = (tick.timestamp_recv - tick.timestamp_exchange) / 1000;
                            println!("📊 Received {} ticks | {}: price={:.2} qty={:.4} latency={}μs", 
                                tick_count, tick.symbol, tick.price, tick.quantity, latency);
                        }
                        let latency = (tick.timestamp_recv - tick.timestamp_exchange) / 1000; // microseconds
                        metrics_clone.record_tick(&tick);
                        rate_tracker_clone.write().await.record_tick(latency as f64);
                    } else {
                        eprintln!("⚠️  Failed to parse tick message");
                    }
                } else {
                    eprintln!("⚠️  Failed to receive message from tick subscriber");
                }
            }
        });
    } else {
        // Retry in background silently
        let metrics_clone = metrics.clone();
        let rate_tracker_clone = rate_tracker.clone();
        tokio::spawn(async move {
            connect_subscriber_with_retry(
                "tcp://localhost:5555",
                "market data stream",
                move |mut tick_sub| {
                    let metrics = metrics_clone.clone();
                    let rate_tracker = rate_tracker_clone.clone();
                    tokio::spawn(async move {
                        let mut tick_count = 0u64;
                        println!("📊 Tick subscriber connected (background retry), waiting for messages...");
                        loop {
                            if let Ok(msg) = tick_sub.recv().await {
                                let msg_bytes: Vec<u8> = msg.try_into().unwrap_or_default();
                                if let Ok(tick) = serde_json::from_slice::<TickEvent>(&msg_bytes) {
                                    tick_count += 1;
                                    if tick_count % 1000 == 0 {
                                        let latency = (tick.timestamp_recv - tick.timestamp_exchange) / 1000;
                                        println!("📊 Received {} ticks | {}: price={:.2} qty={:.4} latency={}μs", 
                                            tick_count, tick.symbol, tick.price, tick.quantity, latency);
                                    }
                                    let latency = (tick.timestamp_recv - tick.timestamp_exchange) / 1000;
                                    metrics.record_tick(&tick);
                                    rate_tracker.write().await.record_tick(latency as f64);
                                }
                            }
                        }
                    });
                }
            ).await;
        });
    }

    if let Some(mut signal_sub) = signal_subscriber {
        let metrics_clone = metrics.clone();
        let rate_tracker_clone = rate_tracker.clone();
        tokio::spawn(async move {
            println!("📥 Signal subscriber task started, waiting for messages...");
            loop {
                if let Ok(msg) = signal_sub.recv().await {
                    let msg_bytes: Vec<u8> = msg.try_into().unwrap_or_default();
                    if let Ok(signal) = serde_json::from_slice::<Signal>(&msg_bytes) {
                        metrics_clone.record_signal(&signal);
                        rate_tracker_clone.write().await.record_signal();
                    } else {
                        eprintln!("⚠️  Failed to parse signal message");
                    }
                }
            }
        });
    }

    if let Some(mut order_sub) = order_subscriber {
        let metrics_clone = metrics.clone();
        let rate_tracker_clone = rate_tracker.clone();
        let recent_orders_clone = recent_orders.clone();
        let orders_by_id_clone = orders_by_id.clone();
        tokio::spawn(async move {
            println!("📥 Order subscriber task started, waiting for messages...");
            loop {
                if let Ok(msg) = order_sub.recv().await {
                    let msg_bytes: Vec<u8> = msg.try_into().unwrap_or_default();
                    match serde_json::from_slice::<OrderRequest>(&msg_bytes) {
                        Ok(order) => {
                        println!("📥 Received order: {} {} {}", order.order_id, order.symbol, order.strategy_name);
                        metrics_clone.record_order(&order);
                        rate_tracker_clone.write().await.record_order();
                        
                        // Store order for later lookup by execution reports
                        {
                            let mut orders_map = orders_by_id_clone.write().await;
                            orders_map.insert(order.order_id.clone(), order.clone());
                            // Keep only last 10000 orders to prevent memory leak
                            if orders_map.len() > 10000 {
                                // Remove oldest entries (simple cleanup - could be improved with LRU)
                                let keys_to_remove: Vec<String> = orders_map.keys().take(1000).cloned().collect();
                                for key in keys_to_remove {
                                    orders_map.remove(&key);
                                }
                            }
                        }
                        
                        let order_event = OrderEvent {
                            order_id: order.order_id.clone(),
                            symbol: order.symbol.clone(),
                            side: format!("{:?}", order.side),
                            quantity: order.quantity,
                            price: order.price,
                            status: "PENDING".to_string(),
                            timestamp: order.timestamp,
                            strategy: order.strategy_name.clone(),
                        };
                        let mut orders = recent_orders_clone.write().await;
                        orders.push_back(order_event);
                        if orders.len() > 1000 {
                            orders.pop_front();
                        }
                        },
                        Err(e) => {
                            eprintln!("⚠️  Failed to parse order message: {}", e);
                            eprintln!("   Message bytes length: {}", msg_bytes.len());
                            if let Ok(text) = String::from_utf8(msg_bytes.clone()) {
                                eprintln!("   Message text: {}", &text[..text.len().min(200)]);
                            }
                        }
                    }
                } else {
                    eprintln!("⚠️  Failed to receive message from order subscriber");
                }
            }
        });
    }

    if let Some(mut exec_sub) = exec_subscriber {
        let metrics_clone = metrics.clone();
        let recent_trades_clone = recent_trades.clone();
        let recent_orders_clone = recent_orders.clone();
        let orders_by_id_clone = orders_by_id.clone();
        tokio::spawn(async move {
            println!("📥 Execution subscriber task started, waiting for messages...");
            loop {
                if let Ok(msg) = exec_sub.recv().await {
                    let msg_bytes: Vec<u8> = msg.try_into().unwrap_or_default();
                    match serde_json::from_slice::<ExecutionReport>(&msg_bytes) {
                        Ok(report) => {
                            println!("📥 Received execution report: {} {:?}", report.order_id, report.status);
                            metrics_clone.record_execution(&report);
                            
                            // Look up the original order to get symbol, side, and strategy_name
                            let order_info = {
                                let orders_map = orders_by_id_clone.read().await;
                                orders_map.get(&report.order_id).cloned()
                            };
                            
                            // Use order info if available, otherwise use defaults
                            let (symbol, side, strategy) = if let Some(order) = order_info.clone() {
                                (
                                    order.symbol,
                                    format!("{:?}", order.side),
                                    order.strategy_name,
                                )
                            } else {
                                // Fallback if order not found (shouldn't happen normally)
                                eprintln!("⚠️  Order not found in cache for order_id: {}", report.order_id);
                                (
                                    "UNKNOWN".to_string(),
                                    "UNKNOWN".to_string(),
                                    "UNKNOWN".to_string(),
                                )
                            };
                            
                            // Update order status in recent_orders
                            {
                                let mut orders = recent_orders_clone.write().await;
                                // Find and update the order status
                                for order_event in orders.iter_mut() {
                                    if order_event.order_id == report.order_id {
                                        order_event.status = format!("{:?}", report.status);
                                        break;
                                    }
                                }
                            }
                            
                            // If filled, add to trades
                            if report.status == OrderStatus::Filled {
                                let trade_event = TradeEvent {
                                    order_id: report.order_id.clone(),
                                    symbol,
                                    side,
                                    quantity: report.filled_qty,
                                    price: report.fill_price.unwrap_or(0.0),
                                    status: format!("{:?}", report.status),
                                    timestamp: report.timestamp,
                                    strategy,
                                };
                                let mut trades = recent_trades_clone.write().await;
                                trades.push_back(trade_event);
                                if trades.len() > 1000 {
                                    trades.pop_front();
                                }
                            }
                        },
                        Err(e) => {
                            eprintln!("⚠️  Failed to parse execution report: {}", e);
                            eprintln!("   Message bytes length: {}", msg_bytes.len());
                            if let Ok(text) = String::from_utf8(msg_bytes.clone()) {
                                eprintln!("   Message text: {}", &text[..text.len().min(200)]);
                            }
                        }
                    }
                } else {
                    eprintln!("⚠️  Failed to receive message from execution subscriber");
                }
            }
        });
    }

    // Spawn task to process candle messages
    if let Some(mut candle_sub) = candle_subscriber {
        let recent_candles_clone = recent_candles.clone();
        tokio::spawn(async move {
            println!("📊 Candlestick subscriber task started, waiting for messages...");
            loop {
                if let Ok(msg) = candle_sub.recv().await {
                    let msg_bytes: Vec<u8> = msg.try_into().unwrap_or_default();
                    match serde_json::from_slice::<CandleStick>(&msg_bytes) {
                        Ok(candle) => {
                            let mut candles = recent_candles_clone.write().await;
                            candles.push_back(candle);
                            if candles.len() > 500 {
                                candles.pop_front();
                            }
                        },
                        Err(e) => {
                            eprintln!("⚠️  Failed to parse candlestick: {}", e);
                        }
                    }
                } else {
                    eprintln!("⚠️  Failed to receive message from candle subscriber");
                }
            }
        });
    }

    // Spawn task to process order book messages
    if let Some(mut book_sub) = book_subscriber {
        let order_books_clone = order_books.clone();
        tokio::spawn(async move {
            println!("📚 Order book subscriber task started, waiting for messages...");
            let mut book_count = 0u64;
            loop {
                if let Ok(msg) = book_sub.recv().await {
                    let msg_bytes: Vec<u8> = msg.try_into().unwrap_or_default();
                    match serde_json::from_slice::<OrderBookSnapshot>(&msg_bytes) {
                        Ok(book) => {
                            book_count += 1;
                            if book_count % 50 == 0 {
                                println!("📚 Received {} order books | {}: mid={:.2} bids={} asks={}", 
                                    book_count, book.symbol, book.mid_price, book.bids.len(), book.asks.len());
                            }
                            let mut books = order_books_clone.write().await;
                            books.insert(book.symbol.clone(), book);
                            // Keep only last 10 symbols to prevent memory leak
                            if books.len() > 10 {
                                // Remove oldest entries (simple cleanup)
                                let keys: Vec<String> = books.keys().cloned().collect();
                                for key in keys.into_iter().take(books.len() - 10) {
                                    books.remove(&key);
                                }
                            }
                        },
                        Err(e) => {
                            eprintln!("⚠️  Failed to parse order book: {}", e);
                        }
                    }
                } else {
                    eprintln!("⚠️  Failed to receive message from order book subscriber");
                }
            }
        });
    } else {
        // Retry in background silently
        let order_books_clone = order_books.clone();
        tokio::spawn(async move {
            connect_subscriber_with_retry(
                "tcp://localhost:5556",
                "order book stream",
                move |mut book_sub| {
                    let order_books = order_books_clone.clone();
                    tokio::spawn(async move {
                        println!("📚 Order book subscriber connected (background retry), waiting for messages...");
                        let mut book_count = 0u64;
                        loop {
                            if let Ok(msg) = book_sub.recv().await {
                                let msg_bytes: Vec<u8> = msg.try_into().unwrap_or_default();
                                match serde_json::from_slice::<OrderBookSnapshot>(&msg_bytes) {
                                    Ok(book) => {
                                        book_count += 1;
                                        if book_count % 50 == 0 {
                                            println!("📚 Received {} order books | {}: mid={:.2} bids={} asks={}", 
                                                book_count, book.symbol, book.mid_price, book.bids.len(), book.asks.len());
                                        }
                                        let mut books = order_books.write().await;
                                        books.insert(book.symbol.clone(), book);
                                        if books.len() > 10 {
                                            let keys: Vec<String> = books.keys().cloned().collect();
                                            for key in keys.into_iter().take(books.len() - 10) {
                                                books.remove(&key);
                                            }
                                        }
                                    },
                                    Err(_) => {}
                                }
                            }
                        }
                    });
                }
            ).await;
        });
    }

    // Choose dashboard mode: TUI or Web
    if use_tui {
        println!("🖥️  Starting TUI Dashboard...");
        println!("   Press 'q' to quit");
        return tui_dashboard::run_tui_dashboard(
            rate_tracker.clone(),
            recent_trades.clone(),
            recent_orders.clone(),
        ).await;
    }

    // Start HTTP server (default)
    let app = Router::new()
        .route("/", get(|| async { Redirect::permanent("/dashboard") }))
        .route("/favicon.ico", get(|| async { (StatusCode::NO_CONTENT, "") }))
        .route("/metrics", get(serve_metrics))
        .route("/dashboard", get(serve_dashboard))
        .route("/api/positions", get(get_positions))
        .route("/api/risk/status", get(get_risk_status))
        .route("/api/risk/kill-switch", post(set_kill_switch))
        .route("/api/risk/limits", post(update_risk_limits))
        .route("/api/trades", get(get_recent_trades))
        .route("/api/orders", get(get_recent_orders))
        .route("/api/metrics/data", get(get_metrics_data))
        .route("/api/backtest", get(get_backtest))
        .route("/api/pnl", get(get_pnl_data))
        .route("/api/strategies", get(get_strategies_status))
        .route("/api/strategies/control", post(control_strategy))
        .route("/api/candles", get(get_candles))
        .route("/api/orderbook", get(get_orderbook))
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(app_state);

    println!("🌐 Starting HTTP server on 0.0.0.0:9090...");
    let listener = match tokio::net::TcpListener::bind("0.0.0.0:9090").await {
        Ok(listener) => {
            println!("✅ HTTP server bound to port 9090");
            listener
        }
        Err(e) => {
            eprintln!("❌ ERROR: Failed to bind to port 9090: {}", e);
            eprintln!("   Make sure no other process is using port 9090");
            return Err(e.into());
        }
    };
    println!("🚀 Server is ready! Access the dashboard at http://localhost:9090/dashboard");
    
    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("❌ ERROR: HTTP server failed: {}", e);
        return Err(e.into());
    }

    Ok(())
}

struct Metrics {
    ticks_total: Counter,
    signals_total: Counter,
    orders_total: Counter,
    fills_total: Counter,
    tick_latency: Histogram,
    #[allow(dead_code)]
    order_latency: Histogram,
    #[allow(dead_code)]
    active_positions: Gauge,
    // HFT Speed and Latency metrics
    ticks_per_second: Gauge,
    signals_per_second: Gauge,
    orders_per_second: Gauge,
    avg_tick_latency_us: Gauge,
    min_tick_latency_us: Gauge,
    max_tick_latency_us: Gauge,
}

impl Metrics {
    fn new(registry: &Registry) -> Self {
        let ticks_total = Counter::new("ticks_total", "Total ticks processed").unwrap();
        let signals_total = Counter::new("signals_total", "Total signals generated").unwrap();
        let orders_total = Counter::new("orders_total", "Total orders sent").unwrap();
        let fills_total = Counter::new("fills_total", "Total fills received").unwrap();
        
        let tick_latency = Histogram::with_opts(
            prometheus::HistogramOpts::new("tick_latency_us", "Tick processing latency in microseconds")
                .buckets(vec![1.0, 5.0, 10.0, 50.0, 100.0, 500.0, 1000.0])
        ).unwrap();
        
        let order_latency = Histogram::with_opts(
            prometheus::HistogramOpts::new("order_latency_us", "Order routing latency in microseconds")
                .buckets(vec![10.0, 50.0, 100.0, 500.0, 1000.0, 5000.0])
        ).unwrap();
        
        let active_positions = Gauge::new("active_positions", "Current active positions").unwrap();
        
        // HFT Speed metrics
        let ticks_per_second = Gauge::new("ticks_per_second", "Ticks processed per second").unwrap();
        let signals_per_second = Gauge::new("signals_per_second", "Signals generated per second").unwrap();
        let orders_per_second = Gauge::new("orders_per_second", "Orders sent per second").unwrap();
        
        // HFT Latency metrics
        let avg_tick_latency_us = Gauge::new("avg_tick_latency_us", "Average tick latency in microseconds").unwrap();
        let min_tick_latency_us = Gauge::new("min_tick_latency_us", "Minimum tick latency in microseconds").unwrap();
        let max_tick_latency_us = Gauge::new("max_tick_latency_us", "Maximum tick latency in microseconds").unwrap();

        registry.register(Box::new(ticks_total.clone())).unwrap();
        registry.register(Box::new(signals_total.clone())).unwrap();
        registry.register(Box::new(orders_total.clone())).unwrap();
        registry.register(Box::new(fills_total.clone())).unwrap();
        registry.register(Box::new(tick_latency.clone())).unwrap();
        registry.register(Box::new(order_latency.clone())).unwrap();
        registry.register(Box::new(active_positions.clone())).unwrap();
        registry.register(Box::new(ticks_per_second.clone())).unwrap();
        registry.register(Box::new(signals_per_second.clone())).unwrap();
        registry.register(Box::new(orders_per_second.clone())).unwrap();
        registry.register(Box::new(avg_tick_latency_us.clone())).unwrap();
        registry.register(Box::new(min_tick_latency_us.clone())).unwrap();
        registry.register(Box::new(max_tick_latency_us.clone())).unwrap();

        Self {
            ticks_total,
            signals_total,
            orders_total,
            fills_total,
            tick_latency,
            order_latency,
            active_positions,
            ticks_per_second,
            signals_per_second,
            orders_per_second,
            avg_tick_latency_us,
            min_tick_latency_us,
            max_tick_latency_us,
        }
    }

    fn record_tick(&self, tick: &TickEvent) {
        self.ticks_total.inc();
        let latency = (tick.timestamp_recv - tick.timestamp_exchange) / 1000; // Convert to microseconds
        self.tick_latency.observe(latency as f64);
    }

    fn record_signal(&self, _signal: &Signal) {
        self.signals_total.inc();
    }

    fn record_order(&self, _order: &OrderRequest) {
        self.orders_total.inc();
    }

    fn record_execution(&self, report: &ExecutionReport) {
        if report.status == OrderStatus::Filled {
            self.fills_total.inc();
        }
    }
}

async fn serve_metrics(
    State(state): State<AppState>
) -> impl IntoResponse {
    let encoder = TextEncoder::new();
    let metric_families = state.registry.gather();
    let mut buffer = vec![];
    encoder.encode(&metric_families, &mut buffer).unwrap();
    
    (
        StatusCode::OK,
        [("Content-Type", "text/plain; version=0.0.4")],
        buffer
    )
}

#[derive(Debug, Serialize)]
struct MetricsData {
    ticks_total: f64,
    signals_total: f64,
    orders_total: f64,
    fills_total: f64,
    // HFT Speed metrics
    ticks_per_second: f64,
    signals_per_second: f64,
    orders_per_second: f64,
    // HFT Latency metrics
    avg_tick_latency_us: f64,
    min_tick_latency_us: f64,
    max_tick_latency_us: f64,
    // Additional metrics
    total_volume: f64,
    sharpe_ratio: f64,
    win_rate: f64,
    avg_trade_pnl: f64,
    total_trades: u64,
    avg_trade_size: f64,
    max_drawdown: f64,
    total_pnl: f64,
}

async fn get_metrics_data(State(state): State<AppState>) -> impl IntoResponse {
    let ticks_total = state.metrics.ticks_total.get();
    let signals_total = state.metrics.signals_total.get();
    let orders_total = state.metrics.orders_total.get();
    let fills_total = state.metrics.fills_total.get();
    
    // Get speed and latency metrics from rate tracker
    let tracker = state.rate_tracker.read().await;
    let ticks_per_second = tracker.get_ticks_per_second();
    let signals_per_second = tracker.get_signals_per_second();
    let orders_per_second = tracker.get_orders_per_second();
    let (avg_tick_latency_us, min_tick_latency_us, max_tick_latency_us) = tracker.get_latency_stats();
    
    // Update gauge metrics for Prometheus
    state.metrics.ticks_per_second.set(ticks_per_second);
    state.metrics.signals_per_second.set(signals_per_second);
    state.metrics.orders_per_second.set(orders_per_second);
    state.metrics.avg_tick_latency_us.set(avg_tick_latency_us);
    state.metrics.min_tick_latency_us.set(min_tick_latency_us);
    state.metrics.max_tick_latency_us.set(max_tick_latency_us);
    
    // Calculate additional metrics from trades
    let trades = state.recent_trades.read().await;
    let mut total_volume = 0.0;
    let mut total_pnl = 0.0;
    let mut winning_trades = 0;
    let mut losing_trades = 0;
    let mut trade_count = 0;
    let mut cumulative_pnl = 0.0;
    let mut peak_pnl = 0.0;
    let mut max_drawdown = 0.0;
    let mut total_trade_size = 0.0;
    
    // Simple P&L calculation: track positions and calculate P&L
    let mut positions: HashMap<String, Vec<(f64, f64, i64)>> = HashMap::new(); // symbol -> vec of (price, quantity, timestamp)
    
    let mut sorted_trades: Vec<_> = trades.iter().collect();
    sorted_trades.sort_by_key(|t| t.timestamp);
    
    for trade in sorted_trades {
        total_volume += trade.quantity * trade.price;
        total_trade_size += trade.quantity;
        trade_count += 1;
        
        let side_is_buy = trade.side.to_uppercase() == "BUY" || trade.side.to_uppercase().contains("BUY");
        let pos_entry = positions.entry(trade.symbol.clone()).or_insert_with(Vec::new);
        
        if side_is_buy {
            pos_entry.push((trade.price, trade.quantity, trade.timestamp));
        } else {
            // Sell - calculate P&L
            let remaining_qty = trade.quantity;
            let mut qty_to_sell = remaining_qty;
            
            // FIFO matching
            while qty_to_sell > 0.0 && !pos_entry.is_empty() {
                let (entry_price, entry_qty, _) = pos_entry[0];
                let qty_to_match = entry_qty.min(qty_to_sell);
                
                let trade_pnl = (trade.price - entry_price) * qty_to_match;
                total_pnl += trade_pnl;
                cumulative_pnl += trade_pnl;
                
                // Track peak and drawdown
                if cumulative_pnl > peak_pnl {
                    peak_pnl = cumulative_pnl;
                }
                let current_drawdown = peak_pnl - cumulative_pnl;
                if current_drawdown > max_drawdown {
                    max_drawdown = current_drawdown;
                }
                
                if trade_pnl > 0.0 {
                    winning_trades += 1;
                } else if trade_pnl < 0.0 {
                    losing_trades += 1;
                }
                
                qty_to_sell -= qty_to_match;
                
                if entry_qty <= qty_to_match {
                    pos_entry.remove(0);
                } else {
                    pos_entry[0] = (entry_price, entry_qty - qty_to_match, pos_entry[0].2);
                }
            }
        }
    }
    
    // Calculate win rate
    let win_rate = if trade_count > 0 {
        (winning_trades as f64 / (winning_trades + losing_trades) as f64) * 100.0
    } else {
        0.0
    };
    
    // Calculate average trade P&L
    let avg_trade_pnl = if trade_count > 0 {
        total_pnl / trade_count as f64
    } else {
        0.0
    };
    
    // Calculate average trade size
    let avg_trade_size = if trade_count > 0 {
        total_trade_size / trade_count as f64
    } else {
        0.0
    };
    
    // Calculate Sharpe ratio (simplified - annualized)
    // For simplicity, use standard deviation of returns
    let sharpe_ratio = if trade_count > 1 && total_pnl != 0.0 {
        // Simplified Sharpe: mean return / std dev (assume 252 trading days)
        let mean_return = total_pnl / trade_count as f64;
        // Very simplified - in production you'd calculate actual std dev
        let std_dev = mean_return.abs() * 0.5; // Simplified approximation
        if std_dev > 0.0 {
            (mean_return / std_dev) * (252.0_f64).sqrt()
        } else {
            0.0
        }
    } else {
        0.0
    };

    Json(MetricsData {
        ticks_total,
        signals_total,
        orders_total,
        fills_total,
        ticks_per_second,
        signals_per_second,
        orders_per_second,
        avg_tick_latency_us,
        min_tick_latency_us,
        max_tick_latency_us,
        total_volume,
        sharpe_ratio: sharpe_ratio.max(-10.0).min(10.0), // Clamp to reasonable range
        win_rate,
        avg_trade_pnl,
        total_trades: trade_count,
        avg_trade_size,
        max_drawdown,
        total_pnl,
    })
}

async fn get_positions(State(state): State<AppState>) -> impl IntoResponse {
    let mut conn = match state.redis_client.get_connection_manager().await {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("⚠️  Failed to get Redis connection for positions: {}. Returning empty list.", e);
            // Return 200 OK with empty array instead of error - dashboard should work even if Redis is down
            return (StatusCode::OK, Json::<Vec<Position>>(vec![]));
        }
    };

    let mut positions = Vec::new();
    
    // Get all position keys (position:SYMBOL)
    if let Ok(keys) = redis::cmd("KEYS").arg("position:*").query_async::<_, Vec<String>>(&mut conn).await {
        for key in keys {
            if let Some(symbol) = key.strip_prefix("position:") {
                if let Ok(quantity) = conn.get::<_, f64>(&key).await {
                    if quantity != 0.0 {
                        positions.push(Position {
                            symbol: symbol.to_string(),
                            quantity,
                            avg_price: None, // Could store this separately
                        });
                    }
                }
            }
        }
    }

    (StatusCode::OK, Json(positions))
}

async fn get_risk_status(State(state): State<AppState>) -> impl IntoResponse {
    let mut conn = match state.redis_client.get_connection_manager().await {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("⚠️  Failed to get Redis connection for risk status: {}. Returning default values.", e);
            // Return 200 OK with default values instead of error - dashboard should work even if Redis is down
            return (StatusCode::OK, Json(RiskStatus {
                kill_switch_active: false,
                daily_loss: 0.0,
                max_daily_loss: 1000.0,
                max_position: 10.0,
                max_order_size: 5.0,
            }));
        }
    };

    let kill_switch_active: bool = conn.get("risk:kill_switch").await.unwrap_or(false);
    let daily_loss: f64 = conn.get("risk:daily_loss").await.unwrap_or(0.0);
    let max_daily_loss: f64 = conn.get("risk:max_daily_loss").await.unwrap_or(1000.0);
    let max_position: f64 = conn.get("risk:max_position").await.unwrap_or(10.0);
    let max_order_size: f64 = conn.get("risk:max_order_size").await.unwrap_or(5.0);

    (StatusCode::OK, Json(RiskStatus {
        kill_switch_active,
        daily_loss,
        max_daily_loss,
        max_position,
        max_order_size,
    }))
}

async fn set_kill_switch(
    State(state): State<AppState>,
    Json(req): Json<KillSwitchRequest>,
) -> impl IntoResponse {
    let mut conn = match state.redis_client.get_connection_manager().await {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("⚠️  Failed to get Redis connection for kill switch: {}. Kill switch cannot be set without Redis.", e);
            // Return 503 Service Unavailable instead of 500 - indicates Redis is not available
            return StatusCode::SERVICE_UNAVAILABLE;
        }
    };

    if let Err(e) = conn.set::<_, _, ()>("risk:kill_switch", req.active).await {
        eprintln!("⚠️  Failed to set kill switch in Redis: {}", e);
        return StatusCode::SERVICE_UNAVAILABLE;
    }

    StatusCode::OK
}

#[derive(Debug, Deserialize)]
struct RiskLimitsRequest {
    max_position: Option<f64>,
    max_order_size: Option<f64>,
    max_daily_loss: Option<f64>,
}

async fn update_risk_limits(
    State(state): State<AppState>,
    Json(req): Json<RiskLimitsRequest>,
) -> impl IntoResponse {
    let mut conn = match state.redis_client.get_connection_manager().await {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("⚠️  Failed to get Redis connection for risk limits: {}", e);
            return StatusCode::SERVICE_UNAVAILABLE;
        }
    };

    if let Some(max_position) = req.max_position {
        if let Err(e) = conn.set::<_, _, ()>("risk:max_position", max_position).await {
            eprintln!("⚠️  Failed to set max_position in Redis: {}", e);
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    }

    if let Some(max_order_size) = req.max_order_size {
        if let Err(e) = conn.set::<_, _, ()>("risk:max_order_size", max_order_size).await {
            eprintln!("⚠️  Failed to set max_order_size in Redis: {}", e);
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    }

    if let Some(max_daily_loss) = req.max_daily_loss {
        if let Err(e) = conn.set::<_, _, ()>("risk:max_daily_loss", max_daily_loss).await {
            eprintln!("⚠️  Failed to set max_daily_loss in Redis: {}", e);
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    }

    StatusCode::OK
}

async fn get_recent_trades(State(state): State<AppState>) -> impl IntoResponse {
    let trades = state.recent_trades.read().await;
    let trades_vec: Vec<TradeEvent> = trades.iter().rev().take(100).cloned().collect();
    Json(trades_vec)
}

async fn get_recent_orders(State(state): State<AppState>) -> impl IntoResponse {
    let orders = state.recent_orders.read().await;
    let orders_vec: Vec<OrderEvent> = orders.iter().rev().take(100).cloned().collect();
    Json(orders_vec)
}

async fn serve_dashboard() -> impl IntoResponse {
    axum::response::Html(include_str!("dashboard.html"))
}

#[derive(Debug, Deserialize)]
struct BacktestParams {
    strategy: Option<String>,
    ema_period: Option<usize>,
    risk_reward: Option<f64>,
    stop_buffer: Option<f64>,
}

#[derive(Debug, Serialize)]
struct PnLData {
    cumulative_pnl: f64,
    pnl_history: Vec<(i64, f64)>, // (timestamp, cumulative_pnl)
}

async fn get_pnl_data(State(state): State<AppState>) -> impl IntoResponse {
    let trades = state.recent_trades.read().await;
    
    // Simple P&L calculation: track positions and calculate P&L
    let mut positions: HashMap<String, Vec<(f64, f64, i64)>> = HashMap::new(); // symbol -> vec of (price, quantity, timestamp)
    let mut pnl_history = Vec::new();
    let mut cumulative_pnl = 0.0;
    
    // Sort trades by timestamp
    let mut sorted_trades: Vec<_> = trades.iter().collect();
    sorted_trades.sort_by_key(|t| t.timestamp);
    
    for trade in sorted_trades {
        let side_is_buy = trade.side.to_uppercase() == "BUY" || trade.side.to_uppercase().contains("BUY");
        let _quantity = if side_is_buy { trade.quantity } else { -trade.quantity };
        
        let pos_entry = positions.entry(trade.symbol.clone()).or_insert_with(Vec::new);
        
        if side_is_buy {
            pos_entry.push((trade.price, trade.quantity, trade.timestamp));
            cumulative_pnl -= trade.price * trade.quantity; // Cost
        } else {
            // Sell - calculate P&L
            let remaining_qty = trade.quantity;
            let mut qty_to_sell = remaining_qty;
            
            // FIFO matching
            while qty_to_sell > 0.0 && !pos_entry.is_empty() {
                let (entry_price, entry_qty, _) = pos_entry[0];
                let qty_to_match = entry_qty.min(qty_to_sell);
                
                let trade_pnl = (trade.price - entry_price) * qty_to_match;
                cumulative_pnl += trade_pnl;
                qty_to_sell -= qty_to_match;
                
                if entry_qty <= qty_to_match {
                    pos_entry.remove(0);
                } else {
                    pos_entry[0] = (entry_price, entry_qty - qty_to_match, pos_entry[0].2);
                }
            }
        }
        
        pnl_history.push((trade.timestamp, cumulative_pnl));
    }
    
    // If we want to include unrealized P&L, we'd need current market prices
    // For now, just return realized P&L
    
    (StatusCode::OK, Json(PnLData {
        cumulative_pnl,
        pnl_history,
    }))
}

async fn get_backtest(Query(params): Query<BacktestParams>) -> impl IntoResponse {
    let _strategy_name = params.strategy.as_deref().unwrap_or("ema_20");
    
    // Load BTC data
    let candles = match backtest::load_btc_data() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to load BTC data: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({
                "error": format!("Failed to load BTC data: {}", e)
            })));
        }
    };

    // Parse start date (8/10/2018 = October 8, 2018)
    let start_date = NaiveDate::parse_from_str("2018-10-08", "%Y-%m-%d")
        .unwrap_or_else(|_| NaiveDate::from_ymd_opt(2018, 10, 8).unwrap());

    // Strategy parameters
    let ema_period = params.ema_period.unwrap_or(20);
    let risk_reward = params.risk_reward.unwrap_or(2.0);
    let stop_buffer = params.stop_buffer.unwrap_or(0.001); // 0.1% buffer

    // Run backtest
    let strategy = backtest::EmaStrategy::new(ema_period, risk_reward, stop_buffer, start_date);
    let result = strategy.backtest(&candles);

    (StatusCode::OK, Json(serde_json::to_value(&result).unwrap_or(serde_json::Value::Null)))
}

#[derive(Debug, Serialize)]
struct StrategyStatus {
    name: String,
    enabled: bool,
    parameters: HashMap<String, String>,
    signals_generated: u64,
    last_signal_time: Option<i64>,
}

async fn get_strategies_status(State(state): State<AppState>) -> impl IntoResponse {
    let mut conn = match state.redis_client.get_connection_manager().await {
        Ok(conn) => conn,
        Err(_) => {
            // Return default strategies if Redis unavailable
            return (StatusCode::OK, Json(vec![
                StrategyStatus {
                    name: "MarketMaker-1".to_string(),
                    enabled: true,
                    parameters: HashMap::new(),
                    signals_generated: 0,
                    last_signal_time: None,
                }
            ]));
        }
    };
    
    // Get strategy status from Redis
    let mut strategies = Vec::new();
    
    // Default strategies
    let strategy_names = vec!["MarketMaker-1", "MeanRev-1", "VWAP-Exec"];
    
    for name in strategy_names {
        let enabled_key = format!("strategy:{}:enabled", name);
        let enabled: bool = conn.get(&enabled_key).await.unwrap_or(true);
        
        let mut parameters = HashMap::new();
        // Get parameters from Redis
        if let Ok(spread) = conn.get::<_, String>(format!("strategy:{}:spread_bps", name)).await {
            parameters.insert("spread_bps".to_string(), spread);
        }
        if let Ok(qty) = conn.get::<_, String>(format!("strategy:{}:quantity", name)).await {
            parameters.insert("quantity".to_string(), qty);
        }
        
        strategies.push(StrategyStatus {
            name: name.to_string(),
            enabled,
            parameters,
            signals_generated: 0,
            last_signal_time: None,
        });
    }
    
    (StatusCode::OK, Json(strategies))
}

#[derive(Debug, Deserialize)]
struct StrategyControlRequest {
    name: String,
    action: String, // "start", "stop", "update"
    parameters: Option<HashMap<String, String>>,
}

async fn control_strategy(
    State(state): State<AppState>,
    Json(req): Json<StrategyControlRequest>,
) -> impl IntoResponse {
    let mut conn = match state.redis_client.get_connection_manager().await {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("⚠️  Failed to get Redis connection for strategy control: {}", e);
            return StatusCode::SERVICE_UNAVAILABLE;
        }
    };
    
    match req.action.as_str() {
        "start" => {
            match conn.set::<_, _, ()>(format!("strategy:{}:enabled", req.name), true).await {
                Ok(_) => StatusCode::OK,
                Err(e) => {
                    eprintln!("Failed to enable strategy: {}", e);
                    StatusCode::INTERNAL_SERVER_ERROR
                }
            }
        }
        "stop" => {
            match conn.set::<_, _, ()>(format!("strategy:{}:enabled", req.name), false).await {
                Ok(_) => StatusCode::OK,
                Err(e) => {
                    eprintln!("Failed to disable strategy: {}", e);
                    StatusCode::INTERNAL_SERVER_ERROR
                }
            }
        }
        "update" => {
            if let Some(params) = req.parameters {
                for (key, value) in params {
                    let param_key = format!("strategy:{}:{}", req.name, key);
                    if let Err(e) = conn.set::<_, _, ()>(param_key, value).await {
                        eprintln!("Failed to update parameter: {}", e);
                        return StatusCode::INTERNAL_SERVER_ERROR;
                    }
                }
            }
            StatusCode::OK
        }
        _ => StatusCode::BAD_REQUEST,
    }
}

async fn get_candles(State(state): State<AppState>) -> impl IntoResponse {
    let candles = state.recent_candles.read().await;
    let candles_vec: Vec<CandleStick> = candles.iter().cloned().collect();
    (StatusCode::OK, Json(candles_vec))
}

#[derive(Debug, Deserialize)]
struct OrderBookParams {
    symbol: Option<String>,
}

async fn get_orderbook(
    Query(params): Query<OrderBookParams>,
    State(state): State<AppState>
) -> impl IntoResponse {
    let books = state.order_books.read().await;
    let symbol = params.symbol.as_deref().unwrap_or("BTC-USD");
    
    if let Some(book) = books.get(symbol) {
        (StatusCode::OK, Json(book.clone()))
    } else {
        // Return empty order book if not found
        let empty_book = OrderBookSnapshot {
            symbol: symbol.to_string(),
            timestamp: 0,
            timestamp_ns: None,
            bids: vec![],
            asks: vec![],
            mid_price: 0.0,
            spread: 0.0,
            depth_level: BookDepth::L2,
        };
        (StatusCode::OK, Json(empty_book))
    }
}