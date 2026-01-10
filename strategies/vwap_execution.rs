use shared::*;
use std::collections::VecDeque;

pub struct VWAPExecution {
    target_quantity: f64,
    executed_quantity: f64,
    vwap_window: VecDeque<(f64, f64)>, // (price, volume)
    window_size: usize,
}

impl VWAPExecution {
    pub fn new(target_quantity: f64, window_size: usize) -> Self {
        Self {
            target_quantity,
            executed_quantity: 0.0,
            vwap_window: VecDeque::with_capacity(window_size),
            window_size,
        }
    }

    pub fn on_book(&mut self, book: &OrderBookSnapshot) -> Vec<Signal> {
        if self.executed_quantity >= self.target_quantity {
            return vec![];
        }

        // Update VWAP calculation
        let volume = book.bids.iter().map(|l| l.quantity).sum::<f64>();
        self.vwap_window.push_back((book.mid_price, volume));
        
        if self.vwap_window.len() > self.window_size {
            self.vwap_window.pop_front();
        }

        // Calculate VWAP
        let (price_vol_sum, vol_sum) = self.vwap_window.iter()
            .fold((0.0, 0.0), |(pv, v), (p, vol)| (pv + p * vol, v + vol));
        
        let vwap = if vol_sum > 0.0 {
            price_vol_sum / vol_sum
        } else {
            book.mid_price
        };

        // Execute if current price is better than VWAP
        if book.mid_price < vwap {
            let slice_size = (self.target_quantity - self.executed_quantity).min(1.0);
            self.executed_quantity += slice_size;

            vec![Signal {
                signal_id: uuid::Uuid::new_v4().to_string(),
                strategy_name: "VWAP-Exec".to_string(),
                symbol: book.symbol.clone(),
                signal_type: SignalType::Buy,
                price: Some(book.mid_price),
                quantity: slice_size,
                timestamp: book.timestamp,
                confidence: 0.9,
            }]
        } else {
            vec![]
        }
    }
}