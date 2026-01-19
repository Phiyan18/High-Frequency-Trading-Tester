use crate::types::*;
use uuid::Uuid;

pub struct SignalConverter;

impl SignalConverter {
    pub fn convert(signal: Signal) -> OrderRequest {
        let order_type = if signal.price.is_some() {
            OrderType::Limit
        } else {
            OrderType::Market
        };

        let side = match signal.signal_type {
            SignalType::Buy => OrderSide::Buy,
            SignalType::Sell => OrderSide::Sell,
            SignalType::Hold => return Self::null_order(),
            SignalType::Cancel => return Self::null_order(),
        };

        OrderRequest {
            order_id: Uuid::new_v4().to_string(),
            signal_id: signal.signal_id,
            symbol: signal.symbol,
            side,
            order_type,
            price: signal.price,
            quantity: signal.quantity,
            timestamp: signal.timestamp,
        }
    }

    fn null_order() -> OrderRequest {
        OrderRequest {
            order_id: String::new(),
            signal_id: String::new(),
            symbol: String::new(),
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            price: None,
            quantity: 0.0,
            timestamp: 0,
        }
    }
}