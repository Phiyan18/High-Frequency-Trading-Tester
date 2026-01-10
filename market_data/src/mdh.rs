// Enhanced Market Data Handler (MDH) Module
// Handles WebSocket connections, order book management, trade tape, and backpressure

use shared::*;
use std::collections::{HashMap, VecDeque, BTreeMap};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, RwLock};

/// Market Data Handler - Core component for processing exchange feeds
pub struct MarketDataHandler {
    order_books: Arc<RwLock<HashMap<String, OrderBook>>>,
    trade_tape: Arc<RwLock<HashMap<String, VecDeque<TradeRecord>>>>,
    event_queue: mpsc::UnboundedSender<MarketDataEvent>,
    latency_tracker: Arc<RwLock<LatencyTracker>>,
    #[allow(dead_code)]
    backpressure_threshold: usize,
}

#[derive(Debug, Clone)]
pub enum MarketDataEvent {
    Tick(TickEvent),
    #[allow(dead_code)]
    OrderBookSnapshot(OrderBookSnapshot),
    #[allow(dead_code)]
    OrderBookUpdate(OrderBookUpdate),
    #[allow(dead_code)]
    Trade(TradeRecord),
}

/// Enhanced Order Book with L1/L2/L3 support
pub struct OrderBook {
    symbol: String,
    bids: BTreeMap<PriceLevel, f64>, // Price -> Quantity (sorted descending)
    asks: BTreeMap<PriceLevel, f64>, // Price -> Quantity (sorted ascending)
    depth_level: BookDepth,
    last_update: u64, // nanoseconds
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct PriceLevel(u64); // Price as fixed-point (multiply by 1e8 for precision)

impl PriceLevel {
    fn from_f64(price: f64) -> Self {
        PriceLevel((price * 1e8) as u64)
    }

    fn to_f64(self) -> f64 {
        self.0 as f64 / 1e8
    }
}

/// Latency tracking for HFT performance monitoring
pub struct LatencyTracker {
    tick_latencies: VecDeque<u64>, // nanoseconds
    #[allow(dead_code)]
    book_latencies: VecDeque<u64>,
    max_samples: usize,
}

impl LatencyTracker {
    pub fn new(max_samples: usize) -> Self {
        Self {
            tick_latencies: VecDeque::with_capacity(max_samples),
            book_latencies: VecDeque::with_capacity(max_samples),
            max_samples,
        }
    }

    pub fn record_tick_latency(&mut self, latency_ns: u64) {
        self.tick_latencies.push_back(latency_ns);
        if self.tick_latencies.len() > self.max_samples {
            self.tick_latencies.pop_front();
        }
    }

    #[allow(dead_code)]
    pub fn record_book_latency(&mut self, latency_ns: u64) {
        self.book_latencies.push_back(latency_ns);
        if self.book_latencies.len() > self.max_samples {
            self.book_latencies.pop_front();
        }
    }

    #[allow(dead_code)]
    pub fn get_avg_tick_latency(&self) -> f64 {
        if self.tick_latencies.is_empty() {
            return 0.0;
        }
        let sum: u64 = self.tick_latencies.iter().sum();
        sum as f64 / self.tick_latencies.len() as f64
    }

    #[allow(dead_code)]
    pub fn get_p99_tick_latency(&self) -> u64 {
        if self.tick_latencies.is_empty() {
            return 0;
        }
        let mut sorted: Vec<u64> = self.tick_latencies.iter().copied().collect();
        sorted.sort_unstable();
        let idx = (sorted.len() as f64 * 0.99) as usize;
        sorted[idx.min(sorted.len() - 1)]
    }
}

impl OrderBook {
    pub fn new(symbol: String, depth_level: BookDepth) -> Self {
        Self {
            symbol,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            depth_level,
            last_update: now_ns(),
        }
    }

    /// Update order book from tick event
    pub fn update_from_tick(&mut self, tick: &TickEvent) {
        self.last_update = now_ns();
        let price_level = PriceLevel::from_f64(tick.price);

        match tick.side {
            Side::Bid => {
                if tick.quantity > 0.0 {
                    self.bids.insert(price_level, tick.quantity);
                } else {
                    self.bids.remove(&price_level);
                }
            }
            Side::Ask => {
                if tick.quantity > 0.0 {
                    self.asks.insert(price_level, tick.quantity);
                } else {
                    self.asks.remove(&price_level);
                }
            }
            Side::Trade => {
                // Trades don't update the order book directly
            }
        }
    }

    /// Get snapshot based on depth level
    pub fn get_snapshot(&self, max_levels: usize) -> OrderBookSnapshot {
        let timestamp_ns = self.last_update;
        let timestamp_ms = timestamp_ns / 1_000_000;

        let (bids, asks) = match self.depth_level {
            BookDepth::L1 => {
                // Only best bid/ask
                let bids: Vec<Level> = self.bids.iter().rev().take(1)
                    .map(|(p, q)| Level { price: p.to_f64(), quantity: *q })
                    .collect();
                let asks: Vec<Level> = self.asks.iter().take(1)
                    .map(|(p, q)| Level { price: p.to_f64(), quantity: *q })
                    .collect();
                (bids, asks)
            }
            BookDepth::L2 | BookDepth::L3 => {
                // Multiple levels (up to max_levels)
                let bids = self.bids.iter().rev().take(max_levels)
                    .map(|(p, q)| Level { price: p.to_f64(), quantity: *q })
                    .collect();
                let asks = self.asks.iter().take(max_levels)
                    .map(|(p, q)| Level { price: p.to_f64(), quantity: *q })
                    .collect();
                (bids, asks)
            }
        };

        let (best_bid, best_ask) = (
            bids.first().map(|l| l.price),
            asks.first().map(|l| l.price),
        );

        let mid_price = match (best_bid, best_ask) {
            (Some(bid), Some(ask)) => (bid + ask) / 2.0,
            (Some(price), None) | (None, Some(price)) => price,
            (None, None) => 0.0,
        };

        let spread = match (best_bid, best_ask) {
            (Some(bid), Some(ask)) => ask - bid,
            _ => 0.0,
        };

        OrderBookSnapshot {
            symbol: self.symbol.clone(),
            timestamp: timestamp_ms as i64,
            timestamp_ns: Some(timestamp_ns),
            bids,
            asks,
            mid_price,
            spread,
            depth_level: self.depth_level.clone(),
        }
    }
}

impl MarketDataHandler {
    pub fn new(
        event_tx: mpsc::UnboundedSender<MarketDataEvent>,
        backpressure_threshold: usize,
    ) -> Self {
        Self {
            order_books: Arc::new(RwLock::new(HashMap::new())),
            trade_tape: Arc::new(RwLock::new(HashMap::new())),
            event_queue: event_tx,
            latency_tracker: Arc::new(RwLock::new(LatencyTracker::new(10000))),
            backpressure_threshold,
        }
    }

    /// Process incoming tick with latency tracking
    pub async fn process_tick(&self, tick: TickEvent) -> Result<(), Box<dyn std::error::Error>> {
        let recv_time_ns = now_ns();
        let latency_ns = recv_time_ns.saturating_sub(tick.timestamp_recv as u64);

        // Update latency tracker
        {
            let mut tracker = self.latency_tracker.write().await;
            tracker.record_tick_latency(latency_ns);
        }

        // Update order book
        {
            let mut books = self.order_books.write().await;
            let book = books.entry(tick.symbol.clone()).or_insert_with(|| {
                OrderBook::new(tick.symbol.clone(), BookDepth::L2)
            });
            book.update_from_tick(&tick);
        }

        // Send to event queue (with backpressure check)
        if self.event_queue.send(MarketDataEvent::Tick(tick)).is_err() {
            eprintln!("⚠️  Event queue full - dropping event (backpressure)");
        }

        Ok(())
    }

    /// Get order book snapshot
    pub async fn get_snapshot(&self, symbol: &str, max_levels: usize) -> Option<OrderBookSnapshot> {
        let books = self.order_books.read().await;
        books.get(symbol).map(|book| book.get_snapshot(max_levels))
    }

    /// Add trade to trade tape (requires symbol from context)
    #[allow(dead_code)]
    pub async fn record_trade(&self, symbol: String, trade: TradeRecord) {
        let mut tape = self.trade_tape.write().await;
        let trades = tape.entry(symbol).or_insert_with(|| {
            VecDeque::with_capacity(1000)
        });
        trades.push_back(trade);
        if trades.len() > 1000 {
            trades.pop_front();
        }
    }

    #[allow(dead_code)]
    pub fn get_latency_tracker(&self) -> Arc<RwLock<LatencyTracker>> {
        self.latency_tracker.clone()
    }
}

/// Get current time in nanoseconds
pub fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

