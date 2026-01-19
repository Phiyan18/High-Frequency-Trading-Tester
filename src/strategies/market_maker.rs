use crate::strategy::Strategy;
use crate::types::*;
use async_trait::async_trait;
use std::collections::{HashMap, VecDeque};

pub struct SimpleMarketMaker {
    name: String,
    spread_bps: f64,           // basis points
    quantity_per_side: f64,
    max_inventory: f64,
    current_inventory: f64,
    last_signal_time: i64,
    min_signal_interval_ms: i64,
    // EMA-related fields
    ema_period: usize,
    price_history: VecDeque<f64>,
    current_ema: Option<f64>,
    // Order book depth analysis
    use_depth: bool,
    depth_levels: usize,
}

impl SimpleMarketMaker {
    pub fn new(name: String) -> Self {
        Self {
            name,
            spread_bps: 8.0,  // Slightly wider spread for better fills
            quantity_per_side: 0.5,  // Smaller size for more frequent trades
            max_inventory: 5.0,
            current_inventory: 0.0,
            last_signal_time: 0,
            min_signal_interval_ms: 50,  // More frequent - every 50ms instead of 100ms
            ema_period: 20,  // Faster EMA for quicker response
            price_history: VecDeque::with_capacity(100),
            current_ema: None,
            use_depth: true,
            depth_levels: 5,  // Analyze top 5 levels
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
            // Initialize with SMA
            let sma: f64 = self.price_history.iter().sum::<f64>() / self.price_history.len() as f64;
            self.current_ema = Some(sma);
            Some(sma)
        }
    }

    fn calculate_orderbook_imbalance(&self, book: &OrderBookSnapshot) -> f64 {
        // Calculate volume imbalance using order book depth
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

    fn calculate_weighted_mid_price(&self, book: &OrderBookSnapshot) -> f64 {
        // Use volume-weighted mid price from top levels
        let bid_weighted: f64 = book.bids.iter()
            .take(self.depth_levels)
            .map(|l| l.price * l.quantity)
            .sum();
        
        let ask_weighted: f64 = book.asks.iter()
            .take(self.depth_levels)
            .map(|l| l.price * l.quantity)
            .sum();
        
        let bid_volume: f64 = book.bids.iter()
            .take(self.depth_levels)
            .map(|l| l.quantity)
            .sum();
        
        let ask_volume: f64 = book.asks.iter()
            .take(self.depth_levels)
            .map(|l| l.quantity)
            .sum();
        
        if bid_volume > 0.0 && ask_volume > 0.0 {
            (bid_weighted / bid_volume + ask_weighted / ask_volume) / 2.0
        } else {
            book.mid_price
        }
    }

    fn adjust_for_inventory(&self, base_bid_qty: f64, base_ask_qty: f64) -> (f64, f64) {
        // Skew quantities based on inventory
        let inventory_ratio = self.current_inventory / self.max_inventory;
        
        let bid_qty = if inventory_ratio > 0.3 {
            base_bid_qty * (1.0 - inventory_ratio * 0.5).max(0.5)
        } else {
            base_bid_qty
        };
        
        let ask_qty = if inventory_ratio < -0.3 {
            base_ask_qty * (1.0 + inventory_ratio.abs() * 0.5).max(0.5)
        } else {
            base_ask_qty
        };
        
        (bid_qty, ask_qty)
    }

    fn adjust_spread_for_imbalance(&self, imbalance: f64) -> f64 {
        // Tighten spread when there's strong imbalance (more aggressive)
        // Positive imbalance = more bids, so tighten ask spread
        // Negative imbalance = more asks, so tighten bid spread
        let adjustment_factor = imbalance.abs() * 0.3; // Max 30% adjustment
        self.spread_bps * (1.0 - adjustment_factor)
    }
}

#[async_trait]
impl Strategy for SimpleMarketMaker {
    fn initialize(&mut self, params: HashMap<String, String>) -> Result<(), String> {
        if let Some(spread) = params.get("spread_bps") {
            self.spread_bps = spread.parse().map_err(|_| "Invalid spread_bps")?;
        }
        if let Some(qty) = params.get("quantity") {
            self.quantity_per_side = qty.parse().map_err(|_| "Invalid quantity")?;
        }
        if let Some(max_inv) = params.get("max_inventory") {
            self.max_inventory = max_inv.parse().map_err(|_| "Invalid max_inventory")?;
        }
        if let Some(ema) = params.get("ema_period") {
            self.ema_period = ema.parse().map_err(|_| "Invalid ema_period")?;
        }
        if let Some(interval) = params.get("min_signal_interval_ms") {
            self.min_signal_interval_ms = interval.parse().map_err(|_| "Invalid min_signal_interval_ms")?;
        }
        if let Some(depth) = params.get("depth_levels") {
            self.depth_levels = depth.parse().map_err(|_| "Invalid depth_levels")?;
        }
        
        println!("✅ Initialized {} with spread={} bps, qty={}, EMA={}, interval={}ms, depth={} levels", 
            self.name, self.spread_bps, self.quantity_per_side, self.ema_period, 
            self.min_signal_interval_ms, self.depth_levels);
        Ok(())
    }

    async fn on_orderbook(&mut self, book: &OrderBookSnapshot) -> Result<Vec<Signal>, String> {
        // Check if we should quote (rate limiting)
        if !self.should_quote(book.timestamp) {
            return Ok(vec![]);
        }

        // Validate order book has data
        if book.bids.is_empty() || book.asks.is_empty() {
            return Ok(vec![]);
        }

        // Calculate order book imbalance
        let imbalance = if self.use_depth {
            self.calculate_orderbook_imbalance(book)
        } else {
            0.0
        };

        // Use weighted mid price for better price discovery
        let reference_price = if self.use_depth {
            self.calculate_weighted_mid_price(book)
        } else {
            book.mid_price
        };

        // Calculate EMA for trend following
        let ema = self.calculate_ema(reference_price);
        
        // Dynamic spread adjustment based on imbalance and EMA
        let mut adjusted_spread_bps = self.spread_bps;
        
        // Adjust for order book imbalance (tighten spread when imbalance is strong)
        adjusted_spread_bps = self.adjust_spread_for_imbalance(imbalance);
        
        // Further adjust based on EMA trend
        if let Some(ema_value) = ema {
            let price_ema_diff = (reference_price - ema_value) / ema_value;
            let trend_adjustment = (price_ema_diff * 0.3).max(-0.3).min(0.3);
            adjusted_spread_bps *= (1.0 - trend_adjustment);
        }
        
        // Ensure minimum spread of 2 bps
        adjusted_spread_bps = adjusted_spread_bps.max(2.0);
        
        // Calculate quote prices
        let half_spread = reference_price * (adjusted_spread_bps / 10000.0);
        let mut bid_price = reference_price - half_spread;
        let mut ask_price = reference_price + half_spread;

        // Skew prices based on imbalance (more aggressive on imbalanced side)
        if imbalance > 0.3 {
            // More bids than asks - be more aggressive on ask (sell higher)
            ask_price += reference_price * (imbalance * 0.0001); // Small adjustment
        } else if imbalance < -0.3 {
            // More asks than bids - be more aggressive on bid (buy lower)
            bid_price -= reference_price * (imbalance.abs() * 0.0001);
        }

        // Round to reasonable precision (2 decimal places for BTC-USDT)
        bid_price = (bid_price * 100.0).round() / 100.0;
        ask_price = (ask_price * 100.0).round() / 100.0;

        // Adjust quantities based on inventory
        let (bid_qty, ask_qty) = self.adjust_for_inventory(self.quantity_per_side, self.quantity_per_side);

        // Calculate confidence based on order book health
        let confidence = {
            let book_health = if book.bids.len() >= 3 && book.asks.len() >= 3 {
                0.9
            } else if book.bids.len() >= 2 && book.asks.len() >= 2 {
                0.7
            } else {
                0.5
            };
            // Increase confidence if we have EMA signal
            if ema.is_some() {
                book_health * 1.1
            } else {
                book_health
            }.min(1.0)
        };

        self.last_signal_time = book.timestamp;

        let mut signals = Vec::new();

        // Generate buy signal
        if bid_qty > 0.001 && bid_price > 0.0 {
            let mut metadata = HashMap::new();
            metadata.insert("imbalance".to_string(), format!("{:.4}", imbalance));
            metadata.insert("spread_bps".to_string(), format!("{:.2}", adjusted_spread_bps));
            if let Some(ema_val) = ema {
                metadata.insert("ema".to_string(), format!("{:.2}", ema_val));
            }

            signals.push(Signal {
                signal_id: uuid::Uuid::new_v4().to_string(),
                strategy_name: self.name.clone(),
                symbol: book.symbol.clone(),
                signal_type: SignalType::Buy,
                price: Some(bid_price),
                quantity: bid_qty,
                timestamp: book.timestamp,
                confidence,
                metadata,
            });
        }

        // Generate sell signal
        if ask_qty > 0.001 && ask_price > 0.0 {
            let mut metadata = HashMap::new();
            metadata.insert("imbalance".to_string(), format!("{:.4}", imbalance));
            metadata.insert("spread_bps".to_string(), format!("{:.2}", adjusted_spread_bps));
            if let Some(ema_val) = ema {
                metadata.insert("ema".to_string(), format!("{:.2}", ema_val));
            }

            signals.push(Signal {
                signal_id: uuid::Uuid::new_v4().to_string(),
                strategy_name: self.name.clone(),
                symbol: book.symbol.clone(),
                signal_type: SignalType::Sell,
                price: Some(ask_price),
                quantity: ask_qty,
                timestamp: book.timestamp,
                confidence,
                metadata,
            });
        }

        Ok(signals)
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn get_state(&self) -> HashMap<String, String> {
        let mut state = HashMap::new();
        state.insert("inventory".to_string(), self.current_inventory.to_string());
        state.insert("spread_bps".to_string(), self.spread_bps.to_string());
        if let Some(ema) = self.current_ema {
            state.insert("ema".to_string(), ema.to_string());
        }
        state
    }

    fn shutdown(&mut self) -> Result<(), String> {
        Ok(())
    }
}

