use shared::*;
use std::collections::HashMap;

pub struct OrderBookBuilder {
    books: HashMap<String, OrderBookSnapshot>,
}

impl OrderBookBuilder {
    pub fn new() -> Self {
        Self {
            books: HashMap::new(),
        }
    }

    pub fn update(&mut self, tick: &TickEvent) {
        let book = self.books.entry(tick.symbol.clone()).or_insert_with(|| {
            OrderBookSnapshot {
                symbol: tick.symbol.clone(),
                timestamp: tick.timestamp_recv,
                timestamp_ns: None,
                bids: Vec::new(),
                asks: Vec::new(),
                mid_price: 0.0,
                spread: 0.0,
                depth_level: BookDepth::L2,
            }
        });

        match tick.side {
            Side::Bid => {
                book.bids.retain(|l| l.price != tick.price);
                if tick.quantity > 0.0 {
                    book.bids.push(Level {
                        price: tick.price,
                        quantity: tick.quantity,
                    });
                    book.bids.sort_by(|a, b| b.price.partial_cmp(&a.price).unwrap());
                    // Keep only top 10 levels
                    if book.bids.len() > 10 {
                        book.bids.truncate(10);
                    }
                }
            }
            Side::Ask => {
                book.asks.retain(|l| l.price != tick.price);
                if tick.quantity > 0.0 {
                    book.asks.push(Level {
                        price: tick.price,
                        quantity: tick.quantity,
                    });
                    book.asks.sort_by(|a, b| a.price.partial_cmp(&b.price).unwrap());
                    // Keep only top 10 levels
                    if book.asks.len() > 10 {
                        book.asks.truncate(10);
                    }
                }
            }
            Side::Trade => {}
        }

        if let (Some(best_bid), Some(best_ask)) = (book.bids.first(), book.asks.first()) {
            book.mid_price = (best_bid.price + best_ask.price) / 2.0;
            book.spread = best_ask.price - best_bid.price;
        }

        book.timestamp = tick.timestamp_recv;
    }

    pub fn get_snapshot(&self, symbol: &str) -> Option<OrderBookSnapshot> {
        self.books.get(symbol).cloned()
    }

    #[allow(dead_code)]
    pub fn get_all_symbols(&self) -> Vec<String> {
        self.books.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_orderbook_builder_new() {
        let builder = OrderBookBuilder::new();
        assert!(builder.books.is_empty());
    }

    #[test]
    fn test_orderbook_builder_update_bid() {
        let mut builder = OrderBookBuilder::new();
        
        let tick = TickEvent {
            symbol: "BTC-USD".to_string(),
            timestamp_exchange: 1000,
            timestamp_recv: 1100,
            side: Side::Bid,
            price: 50000.0,
            quantity: 1.0,
            sequence: 1,
        };

        builder.update(&tick);
        let book = builder.get_snapshot("BTC-USD").unwrap();
        
        assert_eq!(book.bids.len(), 1);
        assert_eq!(book.bids[0].price, 50000.0);
        assert_eq!(book.bids[0].quantity, 1.0);
    }

    #[test]
    fn test_orderbook_builder_update_ask() {
        let mut builder = OrderBookBuilder::new();
        
        let tick = TickEvent {
            symbol: "BTC-USD".to_string(),
            timestamp_exchange: 1000,
            timestamp_recv: 1100,
            side: Side::Ask,
            price: 50100.0,
            quantity: 2.0,
            sequence: 1,
        };

        builder.update(&tick);
        let book = builder.get_snapshot("BTC-USD").unwrap();
        
        assert_eq!(book.asks.len(), 1);
        assert_eq!(book.asks[0].price, 50100.0);
        assert_eq!(book.asks[0].quantity, 2.0);
    }

    #[test]
    fn test_orderbook_builder_calculate_mid_price() {
        let mut builder = OrderBookBuilder::new();
        
        let bid_tick = TickEvent {
            symbol: "BTC-USD".to_string(),
            timestamp_exchange: 1000,
            timestamp_recv: 1100,
            side: Side::Bid,
            price: 50000.0,
            quantity: 1.0,
            sequence: 1,
        };

        let ask_tick = TickEvent {
            symbol: "BTC-USD".to_string(),
            timestamp_exchange: 1001,
            timestamp_recv: 1101,
            side: Side::Ask,
            price: 50100.0,
            quantity: 1.0,
            sequence: 2,
        };

        builder.update(&bid_tick);
        builder.update(&ask_tick);
        
        let book = builder.get_snapshot("BTC-USD").unwrap();
        assert_eq!(book.mid_price, 50050.0);
        assert_eq!(book.spread, 100.0);
    }

    #[test]
    fn test_orderbook_builder_bid_sorting() {
        let mut builder = OrderBookBuilder::new();
        
        // Add bids in random order
        builder.update(&TickEvent {
            symbol: "BTC-USD".to_string(),
            timestamp_exchange: 1000,
            timestamp_recv: 1100,
            side: Side::Bid,
            price: 49800.0,
            quantity: 1.0,
            sequence: 1,
        });

        builder.update(&TickEvent {
            symbol: "BTC-USD".to_string(),
            timestamp_exchange: 1001,
            timestamp_recv: 1101,
            side: Side::Bid,
            price: 50000.0,
            quantity: 1.0,
            sequence: 2,
        });

        builder.update(&TickEvent {
            symbol: "BTC-USD".to_string(),
            timestamp_exchange: 1002,
            timestamp_recv: 1102,
            side: Side::Bid,
            price: 49900.0,
            quantity: 1.0,
            sequence: 3,
        });

        let book = builder.get_snapshot("BTC-USD").unwrap();
        
        // Bids should be sorted descending by price
        assert_eq!(book.bids[0].price, 50000.0);
        assert_eq!(book.bids[1].price, 49900.0);
        assert_eq!(book.bids[2].price, 49800.0);
    }

    #[test]
    fn test_orderbook_builder_ask_sorting() {
        let mut builder = OrderBookBuilder::new();
        
        // Add asks in random order
        builder.update(&TickEvent {
            symbol: "BTC-USD".to_string(),
            timestamp_exchange: 1000,
            timestamp_recv: 1100,
            side: Side::Ask,
            price: 50200.0,
            quantity: 1.0,
            sequence: 1,
        });

        builder.update(&TickEvent {
            symbol: "BTC-USD".to_string(),
            timestamp_exchange: 1001,
            timestamp_recv: 1101,
            side: Side::Ask,
            price: 50100.0,
            quantity: 1.0,
            sequence: 2,
        });

        builder.update(&TickEvent {
            symbol: "BTC-USD".to_string(),
            timestamp_exchange: 1002,
            timestamp_recv: 1102,
            side: Side::Ask,
            price: 50300.0,
            quantity: 1.0,
            sequence: 3,
        });

        let book = builder.get_snapshot("BTC-USD").unwrap();
        
        // Asks should be sorted ascending by price
        assert_eq!(book.asks[0].price, 50100.0);
        assert_eq!(book.asks[1].price, 50200.0);
        assert_eq!(book.asks[2].price, 50300.0);
    }

    #[test]
    fn test_orderbook_builder_zero_quantity_removes_level() {
        let mut builder = OrderBookBuilder::new();
        
        builder.update(&TickEvent {
            symbol: "BTC-USD".to_string(),
            timestamp_exchange: 1000,
            timestamp_recv: 1100,
            side: Side::Bid,
            price: 50000.0,
            quantity: 1.0,
            sequence: 1,
        });

        let book = builder.get_snapshot("BTC-USD").unwrap();
        assert_eq!(book.bids.len(), 1);

        // Update with zero quantity to remove
        builder.update(&TickEvent {
            symbol: "BTC-USD".to_string(),
            timestamp_exchange: 1001,
            timestamp_recv: 1101,
            side: Side::Bid,
            price: 50000.0,
            quantity: 0.0,
            sequence: 2,
        });

        let book = builder.get_snapshot("BTC-USD").unwrap();
        assert_eq!(book.bids.len(), 0);
    }

    #[test]
    fn test_orderbook_builder_multiple_symbols() {
        let mut builder = OrderBookBuilder::new();
        
        builder.update(&TickEvent {
            symbol: "BTC-USD".to_string(),
            timestamp_exchange: 1000,
            timestamp_recv: 1100,
            side: Side::Bid,
            price: 50000.0,
            quantity: 1.0,
            sequence: 1,
        });

        builder.update(&TickEvent {
            symbol: "ETH-USD".to_string(),
            timestamp_exchange: 1001,
            timestamp_recv: 1101,
            side: Side::Bid,
            price: 3000.0,
            quantity: 1.0,
            sequence: 2,
        });

        assert_eq!(builder.get_all_symbols().len(), 2);
        assert!(builder.get_snapshot("BTC-USD").is_some());
        assert!(builder.get_snapshot("ETH-USD").is_some());
    }
}

