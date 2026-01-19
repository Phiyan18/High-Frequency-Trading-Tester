use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBookSnapshot {
    pub symbol: String,
    pub timestamp: i64,
    pub bids: Vec<Level>,
    pub asks: Vec<Level>,
    pub mid_price: f64,
    pub spread: f64,
    #[serde(default)]
    pub imbalance: f64,  // (bid_vol - ask_vol) / (bid_vol + ask_vol) - calculated by strategy if not present
    #[serde(default)]
    pub timestamp_ns: Option<u64>,  // For compatibility with shared::OrderBookSnapshot
    #[serde(default)]
    pub depth_level: Option<String>,  // For compatibility with shared::OrderBookSnapshot
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Level {
    pub price: f64,
    pub quantity: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SignalType {
    Buy,
    Sell,
    Hold,
    Cancel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    pub signal_id: String,
    pub strategy_name: String,
    pub symbol: String,
    pub signal_type: SignalType,
    pub price: Option<f64>,      // None = market order
    pub quantity: f64,
    pub timestamp: i64,
    pub confidence: f64,          // 0.0 - 1.0
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderRequest {
    pub order_id: String,
    pub signal_id: String,
    pub symbol: String,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub price: Option<f64>,
    pub quantity: f64,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OrderSide {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OrderType {
    Market,
    Limit,
    StopLoss,
}

#[derive(Debug, Clone)]
pub struct StrategyMetrics {
    pub signals_generated: u64,
    pub execution_time_us: Vec<u64>,
    pub errors: u64,
}