use shared::*;
use zeromq::{SubSocket, PubSocket, Socket, SocketRecv, SocketSend};
use uuid::Uuid;
use std::collections::VecDeque;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let mut book_subscriber = SubSocket::new();
    book_subscriber.connect("tcp://localhost:5556").await?;
    book_subscriber.subscribe("").await?;

    let mut signal_publisher = PubSocket::new();
    signal_publisher.bind("tcp://0.0.0.0:5557").await?;

    println!("🧠 Strategy Engine started (Improved HFT Market Maker)");
    println!("   Listening: Market Data (books)");
    println!("   Publishing: Signals → tcp://0.0.0.0:5557");
    println!("   Waiting for order book updates...");

    let mut strategy = ImprovedMarketMaker::new();
    let mut book_count = 0;
    let mut signal_count = 0u64;

    loop {
        let msg = book_subscriber.recv().await.map_err(|e| {
            eprintln!("Failed to receive message: {}", e);
            e
        })?;
        let msg_bytes: Vec<u8> = msg.try_into().map_err(|e| {
            eprintln!("Failed to convert message: {}", e);
            e
        })?;
        let book: OrderBookSnapshot = serde_json::from_slice(&msg_bytes).map_err(|e| {
            eprintln!("Failed to deserialize order book: {}", e);
            e
        })?;
        
        book_count += 1;
        if book_count % 50 == 0 {
            println!("📚 Received {} order books | mid_price={:.2} | bids={} | asks={}", 
                book_count, book.mid_price, book.bids.len(), book.asks.len());
        }

        // Skip if order book is invalid or empty
        if book.bids.is_empty() || book.asks.is_empty() || book.mid_price <= 0.0 {
            continue;
        }

        let signals = strategy.on_book(&book);

        for signal in signals {
            signal_count += 1;
            if let Some(price) = signal.price {
                println!("📈 Signal #{}: {} {} {:.4} @ {:.2} (spread={:.2})", 
                    signal_count,
                    signal.strategy_name,
                    match signal.signal_type {
                        SignalType::Buy => "BUY",
                        SignalType::Sell => "SELL",
                    },
                    signal.quantity,
                    price,
                    book.spread
                );
            }

            let msg = serde_json::to_string(&signal)?;
            signal_publisher.send(msg.into()).await?;
        }
    }
}

struct ImprovedMarketMaker {
    last_signal_time: i64,
    min_signal_interval_ms: i64,
    spread_bps: f64,
    quantity_per_side: f64,
    ema_period: usize,
    price_history: VecDeque<f64>,
    current_ema: Option<f64>,
    depth_levels: usize,
}

impl ImprovedMarketMaker {
    fn new() -> Self {
        Self {
            last_signal_time: 0,
            min_signal_interval_ms: 50,  // 50ms minimum interval
            spread_bps: 8.0,  // 8 basis points
            quantity_per_side: 0.5,  // Smaller size for more trades
            ema_period: 20,
            price_history: VecDeque::with_capacity(100),
            current_ema: None,
            depth_levels: 5,
        }
    }

    fn should_quote(&self, timestamp: i64) -> bool {
        (timestamp - self.last_signal_time) >= (self.min_signal_interval_ms * 1_000_000)
    }

    fn calculate_ema(&mut self, price: f64) -> Option<f64> {
        self.price_history.push_back(price);
        if self.price_history.len() > 100 {
            self.price_history.pop_front();
        }

        if self.price_history.len() < self.ema_period {
            return None;
        }

        let multiplier = 2.0 / (self.ema_period as f64 + 1.0);
        
        if let Some(prev_ema) = self.current_ema {
            let new_ema = (price - prev_ema) * multiplier + prev_ema;
            self.current_ema = Some(new_ema);
            Some(new_ema)
        } else {
            let sma: f64 = self.price_history.iter().sum::<f64>() / self.price_history.len() as f64;
            self.current_ema = Some(sma);
            Some(sma)
        }
    }

    fn calculate_orderbook_imbalance(&self, book: &OrderBookSnapshot) -> f64 {
        let bid_volume: f64 = book.bids.iter()
            .take(self.depth_levels)
            .map(|l| l.quantity)
            .sum();
        
        let ask_volume: f64 = book.asks.iter()
            .take(self.depth_levels)
            .map(|l| l.quantity)
            .sum();
        
        let total_volume = bid_volume + ask_volume;
        if total_volume > 0.0 {
            (bid_volume - ask_volume) / total_volume
        } else {
            0.0
        }
    }

    fn on_book(&mut self, book: &OrderBookSnapshot) -> Vec<Signal> {
        // Rate limiting
        if !self.should_quote(book.timestamp) {
            return vec![];
        }

        // Validate order book
        if book.bids.is_empty() || book.asks.is_empty() || book.mid_price <= 0.0 {
            return vec![];
        }

        // Calculate imbalance
        let imbalance = self.calculate_orderbook_imbalance(book);

        // Calculate EMA
        let ema = self.calculate_ema(book.mid_price);
        
        // Dynamic spread adjustment
        let mut adjusted_spread_bps = self.spread_bps;
        
        // Adjust for imbalance (tighten when strong imbalance)
        let imbalance_adjustment = imbalance.abs() * 0.3;
        adjusted_spread_bps *= 1.0 - imbalance_adjustment;
        
        // Adjust based on EMA trend
        if let Some(ema_value) = ema {
            let price_ema_diff = (book.mid_price - ema_value) / ema_value;
            let trend_adjustment = (price_ema_diff * 0.3).max(-0.3).min(0.3);
            adjusted_spread_bps *= 1.0 - trend_adjustment;
        }
        
        // Minimum spread of 2 bps
        adjusted_spread_bps = adjusted_spread_bps.max(2.0);
        
        // Calculate quote prices
        let half_spread = book.mid_price * (adjusted_spread_bps / 10000.0);
        let mut bid_price = book.mid_price - half_spread;
        let mut ask_price = book.mid_price + half_spread;

        // Skew prices based on imbalance
        if imbalance > 0.3 {
            ask_price += book.mid_price * (imbalance * 0.0001);
        } else if imbalance < -0.3 {
            bid_price -= book.mid_price * (imbalance.abs() * 0.0001);
        }

        // Round to 2 decimal places for BTC-USDT
        bid_price = (bid_price * 100.0).round() / 100.0;
        ask_price = (ask_price * 100.0).round() / 100.0;

        // Ensure prices are valid
        if bid_price <= 0.0 || ask_price <= 0.0 || bid_price >= ask_price {
            return vec![];
        }

        self.last_signal_time = book.timestamp;

        vec![
            Signal {
                signal_id: Uuid::new_v4().to_string(),
                strategy_name: "MarketMaker-1".to_string(),
                symbol: book.symbol.clone(),
                signal_type: SignalType::Buy,
                price: Some(bid_price),
                quantity: self.quantity_per_side,
                timestamp: book.timestamp,
            },
            Signal {
                signal_id: Uuid::new_v4().to_string(),
                strategy_name: "MarketMaker-1".to_string(),
                symbol: book.symbol.clone(),
                signal_type: SignalType::Sell,
                price: Some(ask_price),
                quantity: self.quantity_per_side,
                timestamp: book.timestamp,
            },
        ]
    }
}