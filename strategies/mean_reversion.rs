use crate::strategy::Strategy;
use crate::types::*;
use async_trait::async_trait;
use std::collections::{HashMap, VecDeque};

pub struct MeanReversion {
    name: String,
    window_size: usize,
    price_history: VecDeque<f64>,
    entry_threshold: f64,      // z-score threshold
    exit_threshold: f64,
    position: f64,              // current position
    entry_price: Option<f64>,
}

impl MeanReversion {
    pub fn new(name: String, window_size: usize) -> Self {
        Self {
            name,
            window_size,
            price_history: VecDeque::with_capacity(window_size),
            entry_threshold: 2.0,
            exit_threshold: 0.5,
            position: 0.0,
            entry_price: None,
        }
    }

    fn calculate_zscore(&self, current_price: f64) -> Option<f64> {
        if self.price_history.len() < self.window_size {
            return None;
        }

        let mean: f64 = self.price_history.iter().sum::<f64>() / self.price_history.len() as f64;
        let variance: f64 = self.price_history.iter()
            .map(|x| (x - mean).powi(2))
            .sum::<f64>() / self.price_history.len() as f64;
        let std_dev = variance.sqrt();

        if std_dev == 0.0 {
            return None;
        }

        Some((current_price - mean) / std_dev)
    }
}

#[async_trait]
impl Strategy for MeanReversion {
    fn initialize(&mut self, params: HashMap<String, String>) -> Result<(), String> {
        if let Some(threshold) = params.get("entry_threshold") {
            self.entry_threshold = threshold.parse().map_err(|_| "Invalid entry_threshold")?;
        }
        if let Some(exit) = params.get("exit_threshold") {
            self.exit_threshold = exit.parse().map_err(|_| "Invalid exit_threshold")?;
        }
        
        println!("Initialized {} with window={}, entry_z={}", 
            self.name, self.window_size, self.entry_threshold);
        Ok(())
    }

    async fn on_orderbook(&mut self, book: &OrderBookSnapshot) -> Result<Vec<Signal>, String> {
        // Update price history
        self.price_history.push_back(book.mid_price);
        if self.price_history.len() > self.window_size {
            self.price_history.pop_front();
        }

        // Calculate z-score
        let zscore = match self.calculate_zscore(book.mid_price) {
            Some(z) => z,
            None => return Ok(vec![]),  // Not enough data yet
        };

        let mut signals = Vec::new();

        // Entry logic
        if self.position == 0.0 {
            if zscore > self.entry_threshold {
                // Price too high, short
                signals.push(Signal {
                    signal_id: uuid::Uuid::new_v4().to_string(),
                    strategy_name: self.name.clone(),
                    symbol: book.symbol.clone(),
                    signal_type: SignalType::Sell,
                    price: None,  // Market order
                    quantity: 1.0,
                    timestamp: book.timestamp,
                    confidence: (zscore - self.entry_threshold) / self.entry_threshold,
                    metadata: [("zscore".to_string(), zscore.to_string())].iter().cloned().collect(),
                });
                self.position = -1.0;
                self.entry_price = Some(book.mid_price);
            } else if zscore < -self.entry_threshold {
                // Price too low, long
                signals.push(Signal {
                    signal_id: uuid::Uuid::new_v4().to_string(),
                    strategy_name: self.name.clone(),
                    symbol: book.symbol.clone(),
                    signal_type: SignalType::Buy,
                    price: None,
                    quantity: 1.0,
                    timestamp: book.timestamp,
                    confidence: (zscore.abs() - self.entry_threshold) / self.entry_threshold,
                    metadata: [("zscore".to_string(), zscore.to_string())].iter().cloned().collect(),
                });
                self.position = 1.0;
                self.entry_price = Some(book.mid_price);
            }
        }
        // Exit logic
        else if self.position != 0.0 && zscore.abs() < self.exit_threshold {
            let exit_signal = if self.position > 0.0 {
                SignalType::Sell
            } else {
                SignalType::Buy
            };

            signals.push(Signal {
                signal_id: uuid::Uuid::new_v4().to_string(),
                strategy_name: self.name.clone(),
                symbol: book.symbol.clone(),
                signal_type: exit_signal,
                price: None,
                quantity: self.position.abs(),
                timestamp: book.timestamp,
                confidence: 0.9,
                metadata: [("zscore".to_string(), zscore.to_string())].iter().cloned().collect(),
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
        state.insert("history_size".to_string(), self.price_history.len().to_string());
        if let Some(entry) = self.entry_price {
            state.insert("entry_price".to_string(), entry.to_string());
        }
        state
    }

    fn shutdown(&mut self) -> Result<(), String> {
        Ok(())
    }
}