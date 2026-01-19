use crate::strategy::Strategy;
use crate::types::*;
use polars::prelude::*;
use std::collections::HashMap;

pub struct Backtester {
    strategy: Box<dyn Strategy>,
    data: DataFrame,
    signals: Vec<Signal>,
}

impl Backtester {
    pub fn new(strategy: Box<dyn Strategy>, csv_path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let data = CsvReader::from_path(csv_path)?
            .has_header(true)
            .finish()?;
        
        Ok(Self {
            strategy,
            data,
            signals: Vec::new(),
        })
    }

    pub async fn run(&mut self) -> Result<BacktestResults, Box<dyn std::error::Error>> {
        let timestamps = self.data.column("timestamp")?.i64()?;
        let symbols = self.data.column("symbol")?.utf8()?;
        let mid_prices = self.data.column("mid_price")?.f64()?;
        let spreads = self.data.column("spread")?.f64()?;

        for i in 0..self.data.height() {
            let book = OrderBookSnapshot {
                symbol: symbols.get(i).unwrap().to_string(),
                timestamp: timestamps.get(i).unwrap(),
                bids: vec![],  // Simplified
                asks: vec![],
                mid_price: mid_prices.get(i).unwrap(),
                spread: spreads.get(i).unwrap(),
                imbalance: 0.0,
            };

            let signals = self.strategy.on_orderbook(&book).await?;
            self.signals.extend(signals);
        }

        Ok(self.calculate_results())
    }

    fn calculate_results(&self) -> BacktestResults {
        BacktestResults {
            total_signals: self.signals.len(),
            total_trades: 0,  // Calculate from fills
            pnl: 0.0,
            sharpe_ratio: 0.0,
        }
    }
}

#[derive(Debug)]
pub struct BacktestResults {
    pub total_signals: usize,
    pub total_trades: usize,
    pub pnl: f64,
    pub sharpe_ratio: f64,
}