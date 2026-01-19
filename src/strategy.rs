use crate::types::*;
use async_trait::async_trait;
use std::collections::HashMap;

#[async_trait]
pub trait Strategy: Send + Sync {
    /// Initialize strategy with parameters
    fn initialize(&mut self, params: HashMap<String, String>) -> Result<(), String>;
    
    /// Process new order book snapshot
    async fn on_orderbook(&mut self, book: &OrderBookSnapshot) -> Result<Vec<Signal>, String>;
    
    /// Process candlestick data (optional - for candlestick-based strategies)
    async fn on_candlestick(&mut self, candle: &shared::CandleStick) -> Result<Vec<Signal>, String> {
        Ok(vec![])
    }
    
    /// Process trade event (optional)
    async fn on_trade(&mut self, symbol: &str, price: f64, quantity: f64) -> Result<Vec<Signal>, String> {
        Ok(vec![])
    }
    
    /// Periodic callback (e.g., every second)
    async fn on_timer(&mut self) -> Result<Vec<Signal>, String> {
        Ok(vec![])
    }
    
    /// Cleanup on shutdown
    fn shutdown(&mut self) -> Result<(), String> {
        Ok(())
    }
    
    /// Get strategy name
    fn name(&self) -> &str;
    
    /// Get strategy state for monitoring
    fn get_state(&self) -> HashMap<String, String>;
}

/// Strategy loader trait for plugin system
pub trait StrategyLoader {
    fn load_strategy(&self, name: &str, config_path: &str) -> Result<Box<dyn Strategy>, String>;
}