// Paper Trading Execution Engine
// Simulates realistic order execution with slippage and market impact

use shared::*;
use std::collections::HashMap;
use uuid::Uuid;
use rand::Rng;

pub struct PaperTradingEngine {
    positions: HashMap<String, Position>,
    order_books: HashMap<String, OrderBookSnapshot>,
    slippage_bps: f64, // Base slippage in basis points
    market_impact_bps: f64, // Market impact per unit volume
}

struct Position {
    #[allow(dead_code)]
    symbol: String,
    quantity: f64,
    avg_price: f64,
    unrealized_pnl: f64,
}

impl PaperTradingEngine {
    pub fn new(slippage_bps: f64, market_impact_bps: f64) -> Self {
        Self {
            positions: HashMap::new(),
            order_books: HashMap::new(),
            slippage_bps,
            market_impact_bps,
        }
    }

    /// Update market data (order book)
    pub fn update_market_data(&mut self, book: &OrderBookSnapshot) {
        self.order_books.insert(book.symbol.clone(), book.clone());
        
        // Update unrealized P&L for existing positions
        if let Some(position) = self.positions.get_mut(&book.symbol) {
            let current_price = book.mid_price;
            position.unrealized_pnl = (current_price - position.avg_price) * position.quantity;
        }
    }

    /// Execute an order and return execution report
    pub fn execute_order(&mut self, order: &OrderRequest) -> ExecutionReport {
        // Get current market data and clone it to avoid borrow checker issues
        let book = match self.order_books.get(&order.symbol).cloned() {
            Some(b) => b,
            None => {
                return ExecutionReport {
                    order_id: order.order_id.clone(),
                    exec_id: Uuid::new_v4().to_string(),
                    status: OrderStatus::Rejected,
                    filled_qty: 0.0,
                    fill_price: None,
                    timestamp: order.timestamp,
                };
            }
        };

        match order.order_type {
            OrderType::Market => self.execute_market_order(order, &book),
            OrderType::Limit => self.execute_limit_order(order, &book),
        }
    }

    fn execute_market_order(&mut self, order: &OrderRequest, book: &OrderBookSnapshot) -> ExecutionReport {
        let (base_price, available_qty) = match order.side {
            OrderSide::Buy => {
                // Buy at ask price
                if let Some(best_ask) = book.asks.first() {
                    (best_ask.price, best_ask.quantity)
                } else {
                    return ExecutionReport {
                        order_id: order.order_id.clone(),
                        exec_id: Uuid::new_v4().to_string(),
                        status: OrderStatus::Rejected,
                        filled_qty: 0.0,
                        fill_price: None,
                        timestamp: order.timestamp,
                    };
                }
            }
            OrderSide::Sell => {
                // Sell at bid price
                if let Some(best_bid) = book.bids.first() {
                    (best_bid.price, best_bid.quantity)
                } else {
                    return ExecutionReport {
                        order_id: order.order_id.clone(),
                        exec_id: Uuid::new_v4().to_string(),
                        status: OrderStatus::Rejected,
                        filled_qty: 0.0,
                        fill_price: None,
                        timestamp: order.timestamp,
                    };
                }
            }
        };

        // Calculate slippage and market impact
        let mut rng = rand::thread_rng();
        let slippage_multiplier = rng.gen_range(-1.0..1.0); // Random slippage component
        let slippage = self.slippage_bps * (1.0 + slippage_multiplier * 0.5);
        
        // Market impact increases with order size relative to available liquidity
        let liquidity_ratio = (order.quantity / available_qty.max(0.001)).min(1.0);
        let market_impact = self.market_impact_bps * liquidity_ratio;
        
        let total_cost_bps = slippage + market_impact;
        let fill_price = if order.side == OrderSide::Buy {
            base_price * (1.0 + total_cost_bps / 10000.0)
        } else {
            base_price * (1.0 - total_cost_bps / 10000.0)
        };

        let filled_qty = order.quantity.min(available_qty * 0.9); // Fill up to 90% of available liquidity

        // Update position
        self.update_position(&order.symbol, filled_qty, fill_price, &order.side);

        ExecutionReport {
            order_id: order.order_id.clone(),
            exec_id: Uuid::new_v4().to_string(),
            status: if filled_qty >= order.quantity {
                OrderStatus::Filled
            } else {
                OrderStatus::PartiallyFilled
            },
            filled_qty,
            fill_price: Some(fill_price),
            timestamp: order.timestamp + 1000, // Simulate execution latency
        }
    }

    fn execute_limit_order(&mut self, order: &OrderRequest, book: &OrderBookSnapshot) -> ExecutionReport {
        let limit_price = match order.price {
            Some(p) => p,
            None => {
                // No limit price specified, treat as market order
                return self.execute_market_order(order, book);
            }
        };

        let (base_price, available_qty) = match order.side {
            OrderSide::Buy => {
                // Buy limit: can only fill if ask price <= limit price
                if let Some(best_ask) = book.asks.first() {
                    if best_ask.price <= limit_price {
                        (best_ask.price, best_ask.quantity)
                    } else {
                        // Limit price not reached, order pending
                        return ExecutionReport {
                            order_id: order.order_id.clone(),
                            exec_id: Uuid::new_v4().to_string(),
                            status: OrderStatus::Acknowledged,
                            filled_qty: 0.0,
                            fill_price: None,
                            timestamp: order.timestamp,
                        };
                    }
                } else {
                    return ExecutionReport {
                        order_id: order.order_id.clone(),
                        exec_id: Uuid::new_v4().to_string(),
                        status: OrderStatus::Rejected,
                        filled_qty: 0.0,
                        fill_price: None,
                        timestamp: order.timestamp,
                    };
                }
            }
            OrderSide::Sell => {
                // Sell limit: can only fill if bid price >= limit price
                if let Some(best_bid) = book.bids.first() {
                    if best_bid.price >= limit_price {
                        (best_bid.price, best_bid.quantity)
                    } else {
                        // Limit price not reached, order pending
                        return ExecutionReport {
                            order_id: order.order_id.clone(),
                            exec_id: Uuid::new_v4().to_string(),
                            status: OrderStatus::Acknowledged,
                            filled_qty: 0.0,
                            fill_price: None,
                            timestamp: order.timestamp,
                        };
                    }
                } else {
                    return ExecutionReport {
                        order_id: order.order_id.clone(),
                        exec_id: Uuid::new_v4().to_string(),
                        status: OrderStatus::Rejected,
                        filled_qty: 0.0,
                        fill_price: None,
                        timestamp: order.timestamp,
                    };
                }
            }
        };

        // Fill at limit price (or better if market allows)
        let fill_price = match order.side {
            OrderSide::Buy => base_price.min(limit_price),
            OrderSide::Sell => base_price.max(limit_price),
        };

        let filled_qty = order.quantity.min(available_qty * 0.9);

        // Update position
        self.update_position(&order.symbol, filled_qty, fill_price, &order.side);

        ExecutionReport {
            order_id: order.order_id.clone(),
            exec_id: Uuid::new_v4().to_string(),
            status: if filled_qty >= order.quantity {
                OrderStatus::Filled
            } else {
                OrderStatus::PartiallyFilled
            },
            filled_qty,
            fill_price: Some(fill_price),
            timestamp: order.timestamp + 1000,
        }
    }

    fn update_position(&mut self, symbol: &str, qty: f64, price: f64, side: &OrderSide) {
        let position = self.positions.entry(symbol.to_string()).or_insert_with(|| {
            Position {
                symbol: symbol.to_string(),
                quantity: 0.0,
                avg_price: 0.0,
                unrealized_pnl: 0.0,
            }
        });

        match side {
            OrderSide::Buy => {
                let total_cost = position.avg_price * position.quantity + price * qty;
                position.quantity += qty;
                if position.quantity > 0.0 {
                    position.avg_price = total_cost / position.quantity;
                }
            }
            OrderSide::Sell => {
                position.quantity -= qty;
                if position.quantity.abs() < 1e-9 {
                    // Position closed
                    position.quantity = 0.0;
                    position.avg_price = 0.0;
                }
            }
        }
    }

    #[allow(dead_code)]
    pub fn get_positions(&self) -> HashMap<String, (f64, f64, f64)> {
        // Returns (quantity, avg_price, unrealized_pnl) for each symbol
        self.positions
            .iter()
            .filter(|(_, p)| p.quantity.abs() > 1e-9)
            .map(|(symbol, pos)| {
                (symbol.clone(), (pos.quantity, pos.avg_price, pos.unrealized_pnl))
            })
            .collect()
    }

    #[allow(dead_code)]
    pub fn get_total_pnl(&self) -> f64 {
        self.positions.values().map(|p| p.unrealized_pnl).sum()
    }
}

