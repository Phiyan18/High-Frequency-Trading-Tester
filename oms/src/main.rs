use shared::*;
use zeromq::{SubSocket, PubSocket, Socket, SocketRecv, SocketSend};
use sqlx::{PgPool, postgres::PgPoolOptions};
use redis::{AsyncCommands, aio::ConnectionManager};
use std::sync::Arc;
use tokio::sync::RwLock;
use std::collections::HashMap;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect("postgres://hft_user:hft_password@localhost/hft_oms")
        .await?;

    init_database(&pool).await?;

    // Connect to Redis for low-latency cache
    let redis_client = redis::Client::open("redis://127.0.0.1/")?;
    let redis_conn = Arc::new(tokio::sync::Mutex::new(redis_client.get_connection_manager().await?));

    let mut order_subscriber = SubSocket::new();
    order_subscriber.connect("tcp://localhost:5558").await?;
    order_subscriber.subscribe("").await?;

    let mut exec_subscriber = SubSocket::new();
    exec_subscriber.connect("tcp://localhost:5560").await?;
    exec_subscriber.subscribe("").await?;

    let mut exec_publisher = PubSocket::new();
    exec_publisher.bind("tcp://0.0.0.0:5559").await?;
    let exec_publisher = Arc::new(tokio::sync::Mutex::new(exec_publisher));

    let oms = Arc::new(RwLock::new(OrderManagementSystem::new(redis_conn.clone())));

    println!("📋 OMS started");
    println!("   Database: Connected to PostgreSQL");
    println!("   Redis Cache: Connected");
    println!("   Listening: Risk (orders) + Execution (reports)");
    println!("   Publishing: Orders → tcp://0.0.0.0:5559");

    loop {
        tokio::select! {
            Ok(msg) = order_subscriber.recv() => {
                let msg_bytes: Vec<u8> = msg.try_into()?;
                let order: OrderRequest = serde_json::from_slice(&msg_bytes)?;
                
                let oms_clone = oms.clone();
                let redis_clone = redis_conn.clone();
                let exec_pub = exec_publisher.clone();
                let pool_clone = pool.clone();
                
                tokio::spawn(async move {
                    if let Err(e) = oms_clone.write().await.process_order(
                        &order, &pool_clone, redis_clone, exec_pub
                    ).await {
                        eprintln!("Error processing order {}: {}", order.order_id, e);
                    }
                });
            }
            
            Ok(msg) = exec_subscriber.recv() => {
                let msg_bytes: Vec<u8> = msg.try_into()?;
                let report: ExecutionReport = serde_json::from_slice(&msg_bytes)?;
                
                let oms_clone = oms.clone();
                let redis_clone = redis_conn.clone();
                let pool_clone = pool.clone();
                
                tokio::spawn(async move {
                    if let Err(e) = oms_clone.write().await.process_execution(
                        &report, &pool_clone, redis_clone
                    ).await {
                        eprintln!("Error processing execution {}: {}", report.exec_id, e);
                    }
                });
            }
        }
    }
}

struct OrderManagementSystem {
    pending_orders: HashMap<String, OrderRequest>,
    retry_queue: Vec<(OrderRequest, u32)>, // (order, retry_count)
}

impl OrderManagementSystem {
    fn new(_redis: Arc<tokio::sync::Mutex<ConnectionManager>>) -> Self {
        Self {
            pending_orders: HashMap::new(),
            retry_queue: Vec::new(),
        }
    }

    async fn process_order(
        &mut self,
        order: &OrderRequest,
        pool: &PgPool,
        redis: Arc<tokio::sync::Mutex<ConnectionManager>>,
        exec_publisher: Arc<tokio::sync::Mutex<PubSocket>>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // 1. Idempotency check - ensure we haven't processed this order before
        let idempotency_key = format!("order:idempotency:{}", order.order_id);
        let mut conn = redis.lock().await;
        if let Ok(exists) = conn.exists::<_, bool>(&idempotency_key).await {
            if exists {
                drop(conn);
                println!("⚠️  Duplicate order ignored: {}", order.order_id);
                return Ok(());
            }
        }

        // Mark as processed (expire after 24 hours)
        let _: () = conn.set_ex(&idempotency_key, "1", 86400).await?;

        // 2. Generate unique order ID if needed
        let order_id = if order.order_id.is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else {
            order.order_id.clone()
        };

        // 3. Persist to PostgreSQL
        persist_order(pool, order).await?;

        // 4. Cache in Redis for low-latency lookup
        let order_json = serde_json::to_string(order)?;
        let _: () = conn.set_ex(
            &format!("order:cache:{}", order_id),
            &order_json,
            3600
        ).await?;

        // 5. Add audit trail
        let audit_entry = serde_json::json!({
            "order_id": order_id,
            "timestamp": order.timestamp,
            "symbol": order.symbol,
            "side": format!("{:?}", order.side),
            "quantity": order.quantity,
            "price": order.price,
            "status": "NEW",
            "action": "ORDER_CREATED"
        });
        let _: () = conn.lpush("audit:trail", serde_json::to_string(&audit_entry)?).await?;
        let _: () = conn.ltrim("audit:trail", 0, 10000).await?; // Keep last 10k entries
        drop(conn);

        println!("📝 New Order: {} (cached)", order_id);

        // 6. Send to execution engine with retry logic
        self.send_to_execution(order, exec_publisher, 0).await?;
        
        self.pending_orders.insert(order_id, order.clone());
        
        Ok(())
    }

    async fn send_to_execution(
        &mut self,
        order: &OrderRequest,
        exec_publisher: Arc<tokio::sync::Mutex<PubSocket>>,
        retry_count: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let msg = serde_json::to_string(order)?;
        
        // Retry up to 3 times with exponential backoff
        for attempt in 0..=retry_count.max(3) {
            let mut pub_socket = exec_publisher.lock().await;
            match pub_socket.send(msg.clone().into()).await {
                Ok(_) => {
                    return Ok(());
                }
                Err(e) if attempt < 3 => {
                    drop(pub_socket);
                    let backoff_ms = 100 * (1 << attempt); // 100ms, 200ms, 400ms
                    tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
                    eprintln!("⚠️  Retry {}: Failed to send order {}: {}", attempt + 1, order.order_id, e);
                }
                Err(e) => {
                    drop(pub_socket);
                    // Add to retry queue for later processing
                    self.retry_queue.push((order.clone(), retry_count + 1));
                    return Err(format!("Failed to send order after retries: {}", e).into());
                }
            }
        }
        
        Ok(())
    }

    async fn process_execution(
        &mut self,
        report: &ExecutionReport,
        pool: &PgPool,
        redis: Arc<tokio::sync::Mutex<ConnectionManager>>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        println!("📊 Exec Report: {} - {:?}", report.order_id, report.status);

        // 1. Persist execution report
        persist_execution(pool, report).await?;

        // 2. Update order status in database
        update_order_status(pool, report).await?;

        // 3. Update Redis cache
        let mut conn = redis.lock().await;
        if let Ok(order_json) = conn.get::<_, String>(&format!("order:cache:{}", report.order_id)).await {
            if let Ok(_order) = serde_json::from_str::<OrderRequest>(&order_json) {
                // Update cached order status (could update status here if needed)
                conn.set_ex::<_, _, ()>(
                    &format!("order:cache:{}", report.order_id),
                    &order_json,
                    3600
                ).await.ok();
            }
        }

        // 4. Add audit trail entry
        let audit_entry = serde_json::json!({
            "order_id": report.order_id,
            "exec_id": report.exec_id,
            "timestamp": report.timestamp,
            "status": format!("{:?}", report.status),
            "filled_qty": report.filled_qty,
            "fill_price": report.fill_price,
            "action": "EXECUTION_REPORT"
        });
        let _: () = conn.lpush("audit:trail", serde_json::to_string(&audit_entry)?).await?;
        let _: () = conn.ltrim("audit:trail", 0, 10000).await?;
        drop(conn);

        // 5. Remove from pending if filled/cancelled/rejected
        match report.status {
            OrderStatus::Filled | OrderStatus::Cancelled | OrderStatus::Rejected => {
                self.pending_orders.remove(&report.order_id);
            }
            _ => {}
        }

        Ok(())
    }
}

async fn init_database(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS orders (
            order_id TEXT PRIMARY KEY,
            signal_id TEXT,
            symbol TEXT,
            side TEXT,
            order_type TEXT,
            quantity DOUBLE PRECISION,
            price DOUBLE PRECISION,
            status TEXT DEFAULT 'NEW',
            timestamp BIGINT,
            strategy_name TEXT,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
        )
        "#
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS executions (
            exec_id TEXT PRIMARY KEY,
            order_id TEXT,
            status TEXT,
            filled_qty DOUBLE PRECISION,
            fill_price DOUBLE PRECISION,
            timestamp BIGINT,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
        )
        "#
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS audit_trail (
            id SERIAL PRIMARY KEY,
            order_id TEXT,
            action TEXT,
            details JSONB,
            timestamp BIGINT,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
        )
        "#
    )
    .execute(pool)
    .await?;

    Ok(())
}

async fn persist_order(pool: &PgPool, order: &OrderRequest) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO orders (order_id, signal_id, symbol, side, order_type, quantity, price, status, timestamp, strategy_name) 
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         ON CONFLICT (order_id) DO NOTHING"
    )
    .bind(&order.order_id)
    .bind(&order.signal_id)
    .bind(&order.symbol)
    .bind(format!("{:?}", order.side))
    .bind(format!("{:?}", order.order_type))
    .bind(order.quantity)
    .bind(order.price)
    .bind("NEW")
    .bind(order.timestamp)
    .bind(&order.strategy_name)
    .execute(pool)
    .await?;

    Ok(())
}

async fn persist_execution(pool: &PgPool, report: &ExecutionReport) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO executions (exec_id, order_id, status, filled_qty, fill_price, timestamp) 
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (exec_id) DO NOTHING"
    )
    .bind(&report.exec_id)
    .bind(&report.order_id)
    .bind(format!("{:?}", report.status))
    .bind(report.filled_qty)
    .bind(report.fill_price)
    .bind(report.timestamp)
    .execute(pool)
    .await?;

    Ok(())
}

async fn update_order_status(pool: &PgPool, report: &ExecutionReport) -> Result<(), sqlx::Error> {
    let status_str = format!("{:?}", report.status);
    sqlx::query(
        "UPDATE orders SET status = $1 WHERE order_id = $2"
    )
    .bind(&status_str)
    .bind(&report.order_id)
    .execute(pool)
    .await?;

    Ok(())
}
