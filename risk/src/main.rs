use shared::*;
use zeromq::{SubSocket, PubSocket, Socket, SocketRecv, SocketSend};
use std::collections::HashMap;
use redis::AsyncCommands;
use std::sync::Arc;
use tokio::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};


#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    // Connect to Redis
    let redis_client = redis::Client::open("redis://127.0.0.1/")?;
    let redis_conn = Arc::new(tokio::sync::Mutex::new(redis_client.get_connection_manager().await?));

    let mut signal_subscriber = SubSocket::new();
    signal_subscriber.connect("tcp://localhost:5557").await?;
    signal_subscriber.subscribe("").await?;

    let mut order_publisher = PubSocket::new();
    order_publisher.bind("tcp://0.0.0.0:5558").await?;

    println!("🛡️  Risk Management started");
    println!("   Redis: Connected");
    println!("   Listening: Strategies (signals)");
    println!("   Publishing: Orders → tcp://0.0.0.0:5558");

    let risk_manager = Arc::new(RwLock::new(RiskManager::new(redis_conn.clone()).await?));

    // Spawn kill-switch and limits monitor
    let kill_switch_monitor = risk_manager.clone();
    let redis_conn_monitor = redis_conn.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(1));
        loop {
            interval.tick().await;
            let mut rm = kill_switch_monitor.write().await;
            rm.update_daily_loss(redis_conn_monitor.clone()).await;
            rm.refresh_limits(redis_conn_monitor.clone()).await;
        }
    });

    loop {
        let msg = signal_subscriber.recv().await?;
        let msg_bytes: Vec<u8> = msg.try_into()?;
        let signal: Signal = serde_json::from_slice(&msg_bytes)?;

        let order = OrderRequest {
            order_id: uuid::Uuid::new_v4().to_string(),
            signal_id: signal.signal_id.clone(),
            symbol: signal.symbol.clone(),
            side: match signal.signal_type {
                SignalType::Buy => OrderSide::Buy,
                SignalType::Sell => OrderSide::Sell,
            },
            order_type: if signal.price.is_some() {
                OrderType::Limit
            } else {
                OrderType::Market
            },
            price: signal.price,
            quantity: signal.quantity,
            timestamp: signal.timestamp,
            strategy_name: signal.strategy_name.clone(),
        };

        let mut rm = risk_manager.write().await;
        match rm.check_order(&order, redis_conn.clone()).await {
            Ok(RiskDecision::Approved) => {
                println!("✅ APPROVED: {} {} {:.2}", 
                    order.symbol,
                    match order.side {
                        OrderSide::Buy => "BUY",
                        OrderSide::Sell => "SELL",
                    },
                    order.quantity
                );

                let msg = serde_json::to_string(&order)?;
                order_publisher.send(msg.into()).await?;
            }
            Ok(RiskDecision::Rejected(reason)) => {
                println!("❌ REJECTED: {} - {}", order.order_id, reason);
            }
            Err(e) => {
                eprintln!("⚠️  Risk check error: {}", e);
                // Fail-safe: reject on error
                println!("❌ REJECTED (error): {}", order.order_id);
            }
        }
    }
}

#[derive(Debug)]
enum RiskDecision {
    Approved,
    Rejected(String),
}

struct RiskManager {
    positions: HashMap<String, f64>,
    max_position: f64,
    max_order_size: f64,
    max_daily_loss: f64,
    message_count: u64,
    message_window_start: u64,
    max_messages_per_second: u64,
    kill_switch_active: bool,
    price_bands: HashMap<String, (f64, f64)>, // (min, max) price bands
}

impl RiskManager {
    async fn new(redis: Arc<tokio::sync::Mutex<redis::aio::ConnectionManager>>) -> Result<Self, redis::RedisError> {
        // Load kill-switch state from Redis
        let mut conn = redis.lock().await;
        let kill_switch: bool = conn.get("risk:kill_switch").await.unwrap_or(false);
        
        // Load risk limits from Redis (fallback to defaults if not set)
        let max_position: f64 = conn.get("risk:max_position").await.unwrap_or(10.0);
        let max_order_size: f64 = conn.get("risk:max_order_size").await.unwrap_or(5.0);
        let max_daily_loss: f64 = conn.get("risk:max_daily_loss").await.unwrap_or(1000.0);
        
        // Initialize price bands (default: ±10% from last known price)
        let mut price_bands = HashMap::new();
        if let Ok(symbols) = conn.smembers::<_, Vec<String>>("symbols").await {
            for symbol in symbols {
                if let Ok(price) = conn.get::<_, f64>(&format!("price:{}", symbol)).await {
                    price_bands.insert(symbol, (price * 0.9, price * 1.1));
                }
            }
        }

        Ok(Self {
            positions: HashMap::new(),
            max_position,
            max_order_size,
            max_daily_loss,
            message_count: 0,
            message_window_start: now_secs(),
            max_messages_per_second: 100,
            kill_switch_active: kill_switch,
            price_bands,
        })
    }
    
    // Refresh risk limits from Redis (called periodically)
    async fn refresh_limits(&mut self, redis: Arc<tokio::sync::Mutex<redis::aio::ConnectionManager>>) {
        let mut conn = redis.lock().await;
        
        if let Ok(max_position) = conn.get::<_, f64>("risk:max_position").await {
            self.max_position = max_position;
        }
        if let Ok(max_order_size) = conn.get::<_, f64>("risk:max_order_size").await {
            self.max_order_size = max_order_size;
        }
        if let Ok(max_daily_loss) = conn.get::<_, f64>("risk:max_daily_loss").await {
            self.max_daily_loss = max_daily_loss;
        }
    }

    async fn check_order(
        &mut self,
        order: &OrderRequest,
        redis: Arc<tokio::sync::Mutex<redis::aio::ConnectionManager>>,
    ) -> Result<RiskDecision, redis::RedisError> {
        // 1. Kill-switch check (highest priority)
        if self.kill_switch_active {
            return Ok(RiskDecision::Rejected("KILL SWITCH ACTIVE".to_string()));
        }

        // Check Redis for kill-switch
        let mut conn = redis.lock().await;
        if let Ok(active) = conn.get::<_, bool>("risk:kill_switch").await {
            if active {
                self.kill_switch_active = true;
                drop(conn);
                return Ok(RiskDecision::Rejected("KILL SWITCH ACTIVE".to_string()));
            }
        }
        drop(conn);

        // 2. Message rate throttling
        let now = now_secs();
        if now - self.message_window_start >= 1 {
            self.message_count = 0;
            self.message_window_start = now;
        }
        self.message_count += 1;
        if self.message_count > self.max_messages_per_second {
            return Ok(RiskDecision::Rejected("MESSAGE RATE LIMIT EXCEEDED".to_string()));
        }

        // 3. Max order size check
        if order.quantity > self.max_order_size {
            return Ok(RiskDecision::Rejected(format!(
                "ORDER SIZE EXCEEDED: {} > {}",
                order.quantity, self.max_order_size
            )));
        }

        // 4. Price band validation (fat-finger protection)
        if let Some(price) = order.price {
            if let Some((min_price, max_price)) = self.price_bands.get(&order.symbol) {
                if price < *min_price || price > *max_price {
                    return Ok(RiskDecision::Rejected(format!(
                        "PRICE OUT OF BANDS: {} not in [{:.2}, {:.2}]",
                        price, min_price, max_price
                    )));
                }
            }
        }

        // 5. Max open exposure check
        let current_pos = self.positions.get(&order.symbol).copied().unwrap_or(0.0);
        let new_pos = match order.side {
            OrderSide::Buy => current_pos + order.quantity,
            OrderSide::Sell => current_pos - order.quantity,
        };

        if new_pos.abs() > self.max_position {
            return Ok(RiskDecision::Rejected(format!(
                "POSITION LIMIT EXCEEDED: {} > {}",
                new_pos.abs(), self.max_position
            )));
        }

        // 6. Max daily loss check (from Redis) - also refresh limit from Redis
        let mut conn = redis.lock().await;
        if let Ok(max_daily_loss_redis) = conn.get::<_, f64>("risk:max_daily_loss").await {
            self.max_daily_loss = max_daily_loss_redis;
        }
        if let Ok(daily_loss) = conn.get::<_, f64>("risk:daily_loss").await {
            if daily_loss >= self.max_daily_loss {
                drop(conn);
                return Ok(RiskDecision::Rejected("DAILY LOSS LIMIT EXCEEDED".to_string()));
            }
        }

        // All checks passed - update position
        self.positions.insert(order.symbol.clone(), new_pos);

        // Store position in Redis
        let _: () = conn.set(&format!("position:{}", order.symbol), new_pos).await?;
        drop(conn);

        Ok(RiskDecision::Approved)
    }

    async fn update_daily_loss(&mut self, redis: Arc<tokio::sync::Mutex<redis::aio::ConnectionManager>>) {
        // This would be called periodically to update daily loss from execution reports
        // For now, we just check if kill-switch was activated
        let mut conn = redis.lock().await;
        if let Ok(active) = conn.get::<_, bool>("risk:kill_switch").await {
            self.kill_switch_active = active;
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
