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
}

impl SimpleMarketMaker {
    pub fn new(name: String) -> Self {
        Self {
            name,
            spread_bps: 5.0,
            quantity_per_side: 1.0,
            max_inventory: 10.0,
            current_inventory: 0.0,
            last_signal_time: 0,
            min_signal_interval_ms: 100,
            ema_period: 50,
            price_history: VecDeque::with_capacity(100),
            current_ema: None,
        }
    }
    
    fn calculate_ema(&mut self, price: f64) -> Option<f64> {
        self.price_history.push_back(price);
        if self.price_history.len() > self.ema_period * 2 {
            self.price_history.pop_front();
        }
        
        if self.price_history.len() < self.ema_period {
            return None;
        }
        
        let multiplier = 2.0 / (self.ema_period as f64 + 1.0);
        
        // Calculate initial SMA for the first period
        if self.current_ema.is_none() {
            let sma: f64 = self.price_history.iter()
                .rev()
                .take(self.ema_period)
                .sum::<f64>() / self.ema_period as f64;
            self.current_ema = Some(sma);
            return Some(sma);
        }
        
        // Calculate EMA: (Price - EMA_prev) * multiplier + EMA_prev
        let prev_ema = self.current_ema.unwrap();
        let new_ema = (price - prev_ema) * multiplier + prev_ema;
        self.current_ema = Some(new_ema);
        
        Some(new_ema)
    }

    fn calculate_quote_prices(&self, mid_price: f64) -> (f64, f64) {
        let half_spread = mid_price * (self.spread_bps / 10000.0);
        let bid = mid_price - half_spread;
        let ask = mid_price + half_spread;
        (bid, ask)
    }

    fn should_quote(&self, current_time: i64) -> bool {
        (current_time - self.last_signal_time) > (self.min_signal_interval_ms * 1_000_000)
    }

    fn adjust_for_inventory(&self, base_bid_qty: f64, base_ask_qty: f64) -> (f64, f64) {
        // Skew quantities based on inventory
        let inventory_ratio = self.current_inventory / self.max_inventory;
        
        let bid_qty = if inventory_ratio > 0.5 {
            base_bid_qty * (1.0 - inventory_ratio)
        } else {
            base_bid_qty
        };
        
        let ask_qty = if inventory_ratio < -0.5 {
            base_ask_qty * (1.0 + inventory_ratio.abs())
        } else {
            base_ask_qty
        };
        
        (bid_qty, ask_qty)
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
        
        println!("Initialized {} with spread={} bps, EMA period={}", 
            self.name, self.spread_bps, self.ema_period);
        Ok(())
    }

    async fn on_orderbook(&mut self, book: &OrderBookSnapshot) -> Result<Vec<Signal>, String> {
        if !self.should_quote(book.timestamp) {
            return Ok(vec![]);
        }

        // Calculate 50 EMA
        let ema = self.calculate_ema(book.mid_price);
        
        // Use EMA to adjust spread and quote prices
        let (bid_price, ask_price) = if let Some(ema_value) = ema {
            // If price is above EMA, tighten bid spread (be more aggressive buying)
            // If price is below EMA, tighten ask spread (be more aggressive selling)
            let price_ema_diff = (book.mid_price - ema_value) / ema_value;
            let spread_adjustment = (price_ema_diff * 0.5).max(-0.5).min(0.5); // Max 50% adjustment
            let adjusted_spread_bps = self.spread_bps * (1.0 - spread_adjustment);
            let half_spread = book.mid_price * (adjusted_spread_bps / 10000.0);
            (book.mid_price - half_spread, book.mid_price + half_spread)
        } else {
            // Not enough data for EMA yet, use standard calculation
            self.calculate_quote_prices(book.mid_price)
        };
        
        let (bid_qty, ask_qty) = self.adjust_for_inventory(
            self.quantity_per_side,
            self.quantity_per_side,
        );

        let mut signals = Vec::new();
        let mut metadata = HashMap::new();
        
        if let Some(ema_value) = ema {
            metadata.insert("ema_50".to_string(), ema_value.to_string());
            metadata.insert("price_ema_diff".to_string(), 
                ((book.mid_price - ema_value) / ema_value * 10000.0).to_string()); // in bps
        }

        // EMA-based signal generation: only quote aggressively when price is near EMA
        // Only quote if we're not at inventory limits
        if self.current_inventory < self.max_inventory && bid_qty > 0.0 {
            let confidence = if let Some(ema_value) = ema {
                // Higher confidence when price is near or below EMA (good buying opportunity)
                let distance = (book.mid_price - ema_value).abs() / ema_value;
                (1.0 - distance.min(0.05) * 20.0).max(0.5)
            } else {
                0.8
            };
            
            signals.push(Signal {
                signal_id: uuid::Uuid::new_v4().to_string(),
                strategy_name: self.name.clone(),
                symbol: book.symbol.clone(),
                signal_type: SignalType::Buy,
                price: Some(bid_price),
                quantity: bid_qty,
                timestamp: book.timestamp,
                confidence,
                metadata: metadata.clone(),
            });
        }

        if self.current_inventory > -self.max_inventory && ask_qty > 0.0 {
            let confidence = if let Some(ema_value) = ema {
                // Higher confidence when price is near or above EMA (good selling opportunity)
                let distance = (book.mid_price - ema_value).abs() / ema_value;
                (1.0 - distance.min(0.05) * 20.0).max(0.5)
            } else {
                0.8
            };
            
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

        self.last_signal_time = book.timestamp;
        Ok(signals)
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn get_state(&self) -> HashMap<String, String> {
        let mut state = HashMap::new();
        state.insert("inventory".to_string(), self.current_inventory.to_string());
        state.insert("last_signal".to_string(), self.last_signal_time.to_string());
        if let Some(ema) = self.current_ema {
            state.insert("ema_50".to_string(), ema.to_string());
        }
        state.insert("price_history_len".to_string(), self.price_history.len().to_string());
        state
    }

    fn shutdown(&mut self) -> Result<(), String> {
        println!("Shutting down {} with final inventory: {}", self.name, self.current_inventory);
        Ok(())
    }
}