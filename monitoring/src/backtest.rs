use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use chrono::NaiveDate;
use rand::Rng;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candle {
    pub date: NaiveDate,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub date: NaiveDate,
    pub side: String, // "BUY" or "SELL"
    pub price: f64,
    pub quantity: f64,
    pub order_type: String, // "MARKET", "LIMIT", etc.
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestResult {
    pub total_trades: u64,
    pub avg_slippage_bps: f64,
    pub fees_and_rebates: f64,
    pub cost_savings_bps: f64,
    pub fill_rate: f64,
    pub trades: Vec<Trade>,
    pub equity_curve: Vec<(NaiveDate, f64)>,
    pub order_type_distribution: HashMap<String, u64>,
    pub fill_rate_history: Vec<(NaiveDate, f64)>,
    pub slippage_history: Vec<(NaiveDate, f64)>,
}

pub struct EmaStrategy {
    ema_period: usize,
    risk_reward_ratio: f64,
    stop_buffer_pct: f64,
    start_date: NaiveDate,
}

impl EmaStrategy {
    pub fn new(ema_period: usize, risk_reward_ratio: f64, stop_buffer_pct: f64, start_date: NaiveDate) -> Self {
        Self {
            ema_period,
            risk_reward_ratio,
            stop_buffer_pct,
            start_date,
        }
    }

    pub fn calculate_ema(values: &[f64], period: usize) -> Vec<f64> {
        if values.len() < period {
            return vec![];
        }

        let mut ema = Vec::with_capacity(values.len());
        let multiplier = 2.0 / (period as f64 + 1.0);

        // Initialize with SMA
        let sma: f64 = values[0..period].iter().sum::<f64>() / period as f64;
        ema.push(sma);

        // Calculate EMA for remaining values
        for i in period..values.len() {
            let current_ema = (values[i] - ema.last().unwrap()) * multiplier + ema.last().unwrap();
            ema.push(current_ema);
        }

        ema
    }

    pub fn backtest(&self, candles: &[Candle]) -> BacktestResult {
        if candles.len() < self.ema_period {
            return BacktestResult::default();
        }

        let mut rng = rand::thread_rng();
        let mut filtered_candles = Vec::new();
        let start_idx = candles.iter()
            .position(|c| c.date >= self.start_date)
            .unwrap_or(0);

        // Need enough candles before start_date to calculate EMA
        let ema_start_idx = start_idx.saturating_sub(self.ema_period);
        for i in ema_start_idx..candles.len() {
            filtered_candles.push(candles[i].clone());
        }

        if filtered_candles.len() < self.ema_period {
            return BacktestResult::default();
        }

        let closes: Vec<f64> = filtered_candles.iter().map(|c| c.close).collect();
        let ema_values = Self::calculate_ema(&closes, self.ema_period);

        // Align EMA with candles (first ema_period candles don't have EMA)
        let mut trades = Vec::new();
        let mut equity = 100000.0; // Starting capital
        let mut position: Option<(f64, f64, f64)> = None; // (entry_price, quantity, stop_loss)
        let mut equity_curve = Vec::new();
        let mut order_type_dist: HashMap<String, u64> = HashMap::new();
        let mut fills = 0;
        let mut orders = 0;
        let mut total_slippage_bps = 0.0;
        let mut slippage_samples = 0;
        let mut total_fees = 0.0;
        let mut fill_rate_history = Vec::new();
        let mut slippage_history = Vec::new();

        // Start from where we have EMA values
        for i in self.ema_period..filtered_candles.len() {
            let candle = &filtered_candles[i];
            let prev_candle = &filtered_candles[i - 1];
            let ema = ema_values[i - self.ema_period];
            let prev_close = if i > 0 { filtered_candles[i - 1].close } else { candle.open };

            // Check if we should enter/exit based on EMA crossover
            let should_buy = candle.close > ema && prev_close <= ema;
            let should_sell = candle.close < ema && prev_close >= ema;

            // Check trailing stop for existing position
            if let Some((entry_price, quantity, stop_loss)) = position {
                let is_long = quantity > 0.0;
                let current_price = candle.close;
                let new_stop_loss = if is_long {
                    // For long: stop loss is previous candle low + buffer
                    let stop = prev_candle.low * (1.0 - self.stop_buffer_pct);
                    stop.max(stop_loss) // Trailing stop only moves up
                } else {
                    // For short: stop loss is previous candle high + buffer
                    let stop = prev_candle.high * (1.0 + self.stop_buffer_pct);
                    stop.min(stop_loss) // Trailing stop only moves down
                };

                let hit_stop = if is_long {
                    candle.low <= new_stop_loss
                } else {
                    candle.high >= new_stop_loss
                };

                if hit_stop || should_sell {
                    // Exit position
                    orders += 1;
                    let exit_price = if hit_stop {
                        new_stop_loss
                    } else {
                        current_price
                    };

                    // Simulate slippage (small random amount)
                    let slippage_bps = (rng.gen::<f64>() * 5.0) - 2.5; // -2.5 to +2.5 bps
                    let filled_price = exit_price * (1.0 + slippage_bps / 10000.0);
                    total_slippage_bps += slippage_bps.abs();
                    slippage_samples += 1;

                    // Calculate PnL
                    let pnl = if is_long {
                        (filled_price - entry_price) * quantity.abs()
                    } else {
                        (entry_price - filled_price) * quantity.abs()
                    };

                    // Fees (0.1% each way)
                    let fee = filled_price * quantity.abs() * 0.001;
                    total_fees += fee;
                    equity += pnl - fee;

                    trades.push(Trade {
                        date: candle.date,
                        side: if is_long { "SELL".to_string() } else { "BUY".to_string() },
                        price: filled_price,
                        quantity: quantity.abs(),
                        order_type: "MARKET".to_string(),
                    });

                    *order_type_dist.entry("MARKET".to_string()).or_insert(0) += 1;
                    fills += 1;
                    position = None;
                } else {
                    // Update trailing stop
                    position = Some((entry_price, quantity, new_stop_loss));
                }
            } else if should_buy {
                // Enter long position
                let entry_price = candle.close;
                let stop_loss = prev_candle.low * (1.0 - self.stop_buffer_pct);
                let risk = entry_price - stop_loss;
                let reward = risk * self.risk_reward_ratio;
                let _target = entry_price + reward;

                // Position size based on 1% risk
                let risk_amount = equity * 0.01;
                let quantity = risk_amount / risk;

                orders += 1;
                // Simulate slippage
                let slippage_bps = (rng.gen::<f64>() * 5.0) - 2.5;
                let filled_price = entry_price * (1.0 + slippage_bps / 10000.0);
                total_slippage_bps += slippage_bps.abs();
                slippage_samples += 1;

                // Fee
                let fee = filled_price * quantity * 0.001;
                total_fees += fee;
                equity -= fee;

                trades.push(Trade {
                    date: candle.date,
                    side: "BUY".to_string(),
                    price: filled_price,
                    quantity,
                    order_type: "MARKET".to_string(),
                });

                *order_type_dist.entry("MARKET".to_string()).or_insert(0) += 1;
                fills += 1;
                position = Some((filled_price, quantity, stop_loss));
            } else if should_sell && position.is_none() {
                // Enter short position (opposite logic)
                let entry_price = candle.close;
                let stop_loss = prev_candle.high * (1.0 + self.stop_buffer_pct);
                let risk = stop_loss - entry_price;
                let reward = risk * self.risk_reward_ratio;
                let _target = entry_price - reward;

                // Position size
                let risk_amount = equity * 0.01;
                let quantity = -(risk_amount / risk);

                orders += 1;
                let slippage_bps = (rng.gen::<f64>() * 5.0) - 2.5;
                let filled_price = entry_price * (1.0 + slippage_bps / 10000.0);
                total_slippage_bps += slippage_bps.abs();
                slippage_samples += 1;

                let fee = filled_price * quantity.abs() * 0.001;
                total_fees += fee;
                equity -= fee;

                trades.push(Trade {
                    date: candle.date,
                    side: "SELL".to_string(),
                    price: filled_price,
                    quantity: quantity.abs(),
                    order_type: "MARKET".to_string(),
                });

                *order_type_dist.entry("MARKET".to_string()).or_insert(0) += 1;
                fills += 1;
                position = Some((filled_price, quantity, stop_loss));
            }

            // Record equity curve and metrics periodically
            if i % 10 == 0 || i == filtered_candles.len() - 1 {
                equity_curve.push((candle.date, equity));
                
                // Calculate fill rate
                let fill_rate = if orders > 0 { fills as f64 / orders as f64 * 100.0 } else { 0.0 };
                fill_rate_history.push((candle.date, fill_rate));
                
                // Calculate avg slippage
                let avg_slippage = if slippage_samples > 0 {
                    total_slippage_bps / slippage_samples as f64
                } else {
                    0.0
                };
                slippage_history.push((candle.date, avg_slippage));
            }
        }

        // Close any remaining position
        if let Some((entry_price, quantity, _)) = position {
            if let Some(last_candle) = filtered_candles.last() {
                let exit_price = last_candle.close;
                let slippage_bps = (rng.gen::<f64>() * 5.0) - 2.5;
                let filled_price = exit_price * (1.0 + slippage_bps / 10000.0);
                total_slippage_bps += slippage_bps.abs();
                slippage_samples += 1;

                let pnl = if quantity > 0.0 {
                    (filled_price - entry_price) * quantity
                } else {
                    (entry_price - filled_price) * quantity.abs()
                };

                let fee = filled_price * quantity.abs() * 0.001;
                total_fees += fee;
                equity += pnl - fee;

                trades.push(Trade {
                    date: last_candle.date,
                    side: if quantity > 0.0 { "SELL".to_string() } else { "BUY".to_string() },
                    price: filled_price,
                    quantity: quantity.abs(),
                    order_type: "MARKET".to_string(),
                });
                fills += 1;
                orders += 1;
                
                // Record final equity after closing position
                equity_curve.push((last_candle.date, equity));
            }
        }

        let avg_slippage_bps = if slippage_samples > 0 {
            total_slippage_bps / slippage_samples as f64
        } else {
            0.0
        };

        let fill_rate = if orders > 0 {
            fills as f64 / orders as f64 * 100.0
        } else {
            0.0
        };

        // Cost savings vs benchmark (simplified - assume 5 bps benchmark)
        let benchmark_cost = 5.0;
        let cost_savings_bps = benchmark_cost - avg_slippage_bps;

        BacktestResult {
            total_trades: fills as u64,
            avg_slippage_bps,
            fees_and_rebates: total_fees,
            cost_savings_bps,
            fill_rate,
            trades,
            equity_curve,
            order_type_distribution: order_type_dist,
            fill_rate_history,
            slippage_history,
        }
    }
}

impl Default for BacktestResult {
    fn default() -> Self {
        Self {
            total_trades: 0,
            avg_slippage_bps: 0.0,
            fees_and_rebates: 0.0,
            cost_savings_bps: 0.0,
            fill_rate: 0.0,
            trades: Vec::new(),
            equity_curve: Vec::new(),
            order_type_distribution: HashMap::new(),
            fill_rate_history: Vec::new(),
            slippage_history: Vec::new(),
        }
    }
}

pub fn load_btc_data() -> Result<Vec<Candle>, Box<dyn std::error::Error>> {
    use std::fs::File;
    use csv::ReaderBuilder;

    let file = File::open("data/btc.csv")?;
    let mut reader = ReaderBuilder::new()
        .has_headers(true)
        .from_reader(file);

    let mut candles = Vec::new();

    for result in reader.deserialize() {
        #[derive(Debug, Deserialize)]
        struct CsvRow {
            date: String,
            open: f64,
            high: f64,
            low: f64,
            close: f64,
            volume: f64,
        }

        let row: CsvRow = result?;
        let date = NaiveDate::parse_from_str(&row.date, "%Y-%m-%d")?;

        candles.push(Candle {
            date,
            open: row.open,
            high: row.high,
            low: row.low,
            close: row.close,
            volume: row.volume,
        });
    }

    // Sort by date
    candles.sort_by_key(|c| c.date);
    Ok(candles)
}
