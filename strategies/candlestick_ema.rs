use crate::strategy::Strategy;
use crate::types::*;
use async_trait::async_trait;
use std::collections::{HashMap, VecDeque};
use shared::CandleStick;

pub struct CandlestickEmaStrategy {
    name: String,
    fast_period: usize,
    slow_period: usize,
    candles: VecDeque<CandleStick>,
    fast_ema: Option<f64>,
    slow_ema: Option<f64>,
    position: f64,  // 0 = no position, >0 = long, <0 = short
    entry_price: Option<f64>,
    quantity: f64,
}

impl CandlestickEmaStrategy {
    pub fn new(name: String, fast_period: usize, slow_period: usize) -> Self {
        Self {
            name,
            fast_period,
            slow_period,
            candles: VecDeque::with_capacity(slow_period + 10),
            fast_ema: None,
            slow_ema: None,
            position: 0.0,
            entry_price: None,
            quantity: 1.0,
        }
    }

    fn calculate_ema(&self, period: usize) -> Option<f64> {
        if self.candles.len() < period {
            return None;
        }

        let multiplier = 2.0 / (period as f64 + 1.0);
        
        // Start with SMA
        let sma: f64 = self.candles.iter()
            .rev()
            .take(period)
            .map(|c| c.close)
            .sum::<f64>() / period as f64;

        // Calculate EMA iteratively
        let mut ema = sma;
        for candle in self.candles.iter().rev().take(period) {
            ema = (candle.close - ema) * multiplier + ema;
        }

        Some(ema)
    }

    fn update_emas(&mut self) {
        self.fast_ema = self.calculate_ema(self.fast_period);
        self.slow_ema = self.calculate_ema(self.slow_period);
    }
}

#[async_trait]
impl Strategy for CandlestickEmaStrategy {
    fn initialize(&mut self, params: HashMap<String, String>) -> Result<(), String> {
        if let Some(qty) = params.get("quantity") {
            self.quantity = qty.parse().map_err(|_| "Invalid quantity")?;
        }
        if let Some(fast) = params.get("fast_period") {
            self.fast_period = fast.parse().map_err(|_| "Invalid fast_period")?;
        }
        if let Some(slow) = params.get("slow_period") {
            self.slow_period = slow.parse().map_err(|_| "Invalid slow_period")?;
        }
        
        println!("Initialized {} with fast_ema={}, slow_ema={}, qty={}", 
            self.name, self.fast_period, self.slow_period, self.quantity);
        Ok(())
    }

    async fn on_orderbook(&mut self, _book: &OrderBookSnapshot) -> Result<Vec<Signal>, String> {
        // This strategy works on candlesticks, not order books
        Ok(vec![])
    }

    async fn on_candlestick(&mut self, candle: &CandleStick) -> Result<Vec<Signal>, String> {
        // Add new candle
        self.candles.push_back(candle.clone());
        if self.candles.len() > self.slow_period + 10 {
            self.candles.pop_front();
        }

        // Update EMAs
        self.update_emas();

        let fast_ema = match self.fast_ema {
            Some(ema) => ema,
            None => return Ok(vec![]), // Not enough data
        };

        let slow_ema = match self.slow_ema {
            Some(ema) => ema,
            None => return Ok(vec![]), // Not enough data
        };

        let mut signals = Vec::new();

        // Get previous EMAs for crossover detection
        let prev_candles: Vec<_> = self.candles.iter().rev().take(2).collect();
        if prev_candles.len() < 2 {
            return Ok(vec![]);
        }

        // Calculate previous EMAs (simplified - in production, maintain EMA history)
        let prev_close = prev_candles[1].close;
        let prev_fast_ema = fast_ema - (candle.close - prev_close) * (2.0 / (self.fast_period as f64 + 1.0));
        let prev_slow_ema = slow_ema - (candle.close - prev_close) * (2.0 / (self.slow_period as f64 + 1.0));

        // Detect crossover
        let bullish_cross = fast_ema > slow_ema && prev_fast_ema <= prev_slow_ema;
        let bearish_cross = fast_ema < slow_ema && prev_fast_ema >= prev_slow_ema;

        // Entry signals
        if self.position == 0.0 {
            if bullish_cross {
                // Golden cross - buy signal
                signals.push(Signal {
                    signal_id: uuid::Uuid::new_v4().to_string(),
                    strategy_name: self.name.clone(),
                    symbol: candle.symbol.clone(),
                    signal_type: SignalType::Buy,
                    price: Some(candle.close),  // Limit order at close
                    quantity: self.quantity,
                    timestamp: candle.timestamp,
                    confidence: 0.8,
                    metadata: [
                        ("fast_ema".to_string(), fast_ema.to_string()),
                        ("slow_ema".to_string(), slow_ema.to_string()),
                        ("crossover".to_string(), "bullish".to_string()),
                    ].iter().cloned().collect(),
                });
                self.position = self.quantity;
                self.entry_price = Some(candle.close);
            } else if bearish_cross {
                // Death cross - sell signal
                signals.push(Signal {
                    signal_id: uuid::Uuid::new_v4().to_string(),
                    strategy_name: self.name.clone(),
                    symbol: candle.symbol.clone(),
                    signal_type: SignalType::Sell,
                    price: Some(candle.close),  // Limit order at close
                    quantity: self.quantity,
                    timestamp: candle.timestamp,
                    confidence: 0.8,
                    metadata: [
                        ("fast_ema".to_string(), fast_ema.to_string()),
                        ("slow_ema".to_string(), slow_ema.to_string()),
                        ("crossover".to_string(), "bearish".to_string()),
                    ].iter().cloned().collect(),
                });
                self.position = -self.quantity;
                self.entry_price = Some(candle.close);
            }
        }
        // Exit signals - opposite crossover
        else if self.position > 0.0 && bearish_cross {
            // Exit long position
            signals.push(Signal {
                signal_id: uuid::Uuid::new_v4().to_string(),
                strategy_name: self.name.clone(),
                symbol: candle.symbol.clone(),
                signal_type: SignalType::Sell,
                price: Some(candle.close),
                quantity: self.position,
                timestamp: candle.timestamp,
                confidence: 0.9,
                metadata: [
                    ("fast_ema".to_string(), fast_ema.to_string()),
                    ("slow_ema".to_string(), slow_ema.to_string()),
                    ("action".to_string(), "exit_long".to_string()),
                ].iter().cloned().collect(),
            });
            self.position = 0.0;
            self.entry_price = None;
        } else if self.position < 0.0 && bullish_cross {
            // Exit short position
            signals.push(Signal {
                signal_id: uuid::Uuid::new_v4().to_string(),
                strategy_name: self.name.clone(),
                symbol: candle.symbol.clone(),
                signal_type: SignalType::Buy,
                price: Some(candle.close),
                quantity: self.position.abs(),
                timestamp: candle.timestamp,
                confidence: 0.9,
                metadata: [
                    ("fast_ema".to_string(), fast_ema.to_string()),
                    ("slow_ema".to_string(), slow_ema.to_string()),
                    ("action".to_string(), "exit_short".to_string()),
                ].iter().cloned().collect(),
            });
            self.position = 0.0;
            self.entry_price = None;
        }

        Ok(signals)
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn get_state(&self) -> HashMap<String, String> {
        let mut state = HashMap::new();
        state.insert("position".to_string(), self.position.to_string());
        state.insert("candles_count".to_string(), self.candles.len().to_string());
        if let Some(entry) = self.entry_price {
            state.insert("entry_price".to_string(), entry.to_string());
        }
        if let Some(fast) = self.fast_ema {
            state.insert("fast_ema".to_string(), fast.to_string());
        }
        if let Some(slow) = self.slow_ema {
            state.insert("slow_ema".to_string(), slow.to_string());
        }
        state
    }

    fn shutdown(&mut self) -> Result<(), String> {
        Ok(())
    }
}

