# HFT Market Data Platform

A high-performance, production-grade High-Frequency Trading (HFT) market data platform built in Rust. This platform provides a complete end-to-end trading system with market data processing, strategy execution, risk management, order management, execution simulation, and real-time monitoring.

## 🚀 Overview

This platform is designed for low-latency algorithmic trading, featuring microsecond-level message passing, comprehensive risk controls, and real-time observability. The system is built using a microservices architecture with ZeroMQ for inter-process communication, ensuring high throughput and low latency.

### Key Features

- **Ultra-Fast Market Data Processing**: CSV tick stream replay with order book building (L2 with top 10 levels)
- **Strategy Engine**: Plugin-based runtime with multiple built-in strategies (Market Maker, Mean Reversion, VWAP)
- **Risk Management**: Pre-trade risk controls including position limits, daily loss limits, and kill-switch
- **Order Management System (OMS)**: PostgreSQL-backed order state management with Redis caching
- **Execution Engine**: Simulated exchange matching engine with configurable latency
- **Real-Time Monitoring**: Prometheus metrics and web-based dashboard
- **Security Layer**: JWT-based authentication with role-based access control

## 📋 Table of Contents

- [Architecture](#architecture)
- [Technology Stack](#technology-stack)
- [Installation](#installation)
- [Configuration](#configuration)
- [Quick Start](#quick-start)
- [System Components](#system-components)
- [API Endpoints](#api-endpoints)
- [Project Structure](#project-structure)
- [Development](#development)
- [Testing](#testing)

## 🏗️ Architecture

The platform follows a microservices architecture with the following components:

```
┌──────────────┐
│ Market Data  │ (CSV Replay → ZeroMQ PUB)
└──────┬───────┘
       │ Ticks (5555) & Books (5556)
       ▼
┌──────────────┐
│  Strategies  │ (Generates Trading Signals)
└──────┬───────┘
       │ Signals (5557)
       ▼
┌──────────────┐
│    Risk      │ (Pre-Trade Risk Checks)
└──────┬───────┘
       │ Orders (5558)
       ▼
┌──────────────┐
│     OMS      │ (Order State Management)
└──────┬───────┘
       │ Orders (5559)
       ▼
┌──────────────┐
│  Execution   │ (Exchange Simulator)
└──────┬───────┘
       │ Execution Reports (5560)
       ▼
┌──────────────┐
│ Monitoring   │ (Metrics & Dashboard)
└──────────────┘
```

### Component Communication

- **ZeroMQ PUB-SUB**: Used for one-to-many message distribution (market data, signals, orders)
- **ZeroMQ REQ-REP**: Used for request-response patterns (risk checks)
- **Redis**: Shared state storage (positions, risk limits, kill-switch)
- **PostgreSQL**: Persistent order storage and audit trail

## 🛠️ Technology Stack

| Component | Technology |
|-----------|-----------|
| **Language** | Rust (all services) |
| **Async Runtime** | Tokio |
| **IPC/Networking** | ZeroMQ (PUB-SUB, REQ-REP) |
| **Time Handling** | Chrono (nanosecond precision) |
| **Storage** | PostgreSQL (OMS), Redis (Cache/State) |
| **Metrics** | Prometheus |
| **Web Framework** | Axum |
| **Authentication** | JWT (jsonwebtoken) |
| **Serialization** | Serde (JSON) |
| **Logging** | Tracing / tracing-subscriber |

## 📦 Installation

### Prerequisites

1. **Rust** (latest stable version)
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```

2. **Redis** (for state management)
   - Windows: Download from [Redis for Windows](https://github.com/microsoftarchive/redis/releases) or use WSL
   - Linux/Mac: `sudo apt-get install redis-server` or `brew install redis`

3. **PostgreSQL** (for OMS persistence)
   - Windows: Download from [PostgreSQL Downloads](https://www.postgresql.org/download/windows/)
   - Linux: `sudo apt-get install postgresql postgresql-contrib`
   - Mac: `brew install postgresql`

### Setup Steps

1. **Clone the repository**
   ```bash
   git clone <repository-url>
   cd hft_market_data
   ```

2. **Start Redis**
   ```bash
   redis-server
   # Or on Windows (if installed via WSL):
   wsl redis-server
   ```

3. **Setup PostgreSQL Database**
   ```bash
   # Create database
   createdb hft_oms
   
   # Or via psql:
   psql -U postgres
   CREATE DATABASE hft_oms;
   ```

4. **Update Database Connection** (if needed)
   Edit `config/platform.toml`:
   ```toml
   [oms]
   database_url = "postgres://user:password@localhost/hft_oms"
   ```

5. **Prepare Market Data**
   Ensure you have `data/market_data.csv` and `data/btc.csv` files in the `data/` directory.

6. **Build the Project**
   ```bash
   cargo build --release
   ```

## ⚙️ Configuration

Configuration is managed via `config/platform.toml`. Key settings:

### Market Data
```toml
[market_data]
replay_speed = 1.0  # 1.0 = real-time, 10.0 = 10x speed
tick_endpoint = "tcp://*:5555"
book_endpoint = "tcp://*:5556"
```

### Strategies
```toml
[strategies]
enabled = ["MarketMaker-1", "MeanRev-1", "VWAP-Exec"]

[[strategies.configs]]
name = "MarketMaker-1"
spread_bps = 10          # Spread in basis points
quantity = 1.0           # Order quantity
max_inventory = 10.0     # Maximum inventory
```

### Risk Management
```toml
[risk]
max_position_per_symbol = 10.0      # Maximum position per symbol
max_order_size = 5.0                # Maximum order size
max_daily_loss = -10000.0           # Maximum daily loss limit
max_orders_per_second = 100         # Rate limiting
price_band_min = 0.01               # Minimum price (fat-finger protection)
price_band_max = 1000000.0          # Maximum price
```

### Execution Engine
```toml
[execution]
simulation_mode = true
latency_min_us = 50      # Minimum simulated latency (microseconds)
latency_max_us = 500     # Maximum simulated latency (microseconds)
```

### Monitoring
```toml
[monitoring]
prometheus_port = 9090
dashboard_port = 9091
log_level = "info"
```

## 🚀 Quick Start

### Windows

Use the provided PowerShell script:

```powershell
.\start.ps1
```

This script will:
1. Check for Redis and PostgreSQL
2. Stop any existing services
3. Start all services in the correct order
4. Display service logs
5. Provide dashboard URLs

### Linux/Mac

Start services manually (or create a similar shell script):

```bash
# Terminal 1: Market Data
cargo run --release --bin market_data &

# Terminal 2: Strategies
cargo run --release --bin strategies &

# Terminal 3: Risk Management
cargo run --release --bin risk &

# Terminal 4: OMS
cargo run --release --bin oms &

# Terminal 5: Execution
cargo run --release --bin execution &

# Terminal 6: Monitoring
cargo run --release --bin monitoring &

# Terminal 7: Security (optional)
cargo run --release --bin security &
```

### Access Dashboards

- **Monitoring Dashboard**: http://localhost:9090/dashboard
- **Prometheus Metrics**: http://localhost:9090/metrics
- **Security Token Service**: http://localhost:9091/token

## 🔧 System Components

### 1. Market Data Handler (`market_data`)

Processes CSV tick data and builds order books.

**Features:**
- CSV tick stream replay with configurable speed
- L2 order book builder (top 10 levels)
- ZeroMQ PUB-SUB distribution
- Nanosecond timestamp precision
- Latency tracking (exchange vs receive time)

**Ports:**
- `5555`: Tick stream
- `5556`: Order book snapshots

### 2. Strategy Engine (`strategies`)

Plugin-based strategy runtime that generates trading signals.

**Built-in Strategies:**
- **Market Maker**: Provides liquidity with configurable spreads
- **Mean Reversion**: Trades on price mean reversion
- **VWAP Execution**: Volume-weighted average price execution

**Features:**
- Hot-registration of strategies
- ZeroMQ-based signal distribution
- Strategy metrics tracking
- Signal → Order conversion

**Ports:**
- `5557`: Signal stream (output)

### 3. Risk Management (`risk`)

Pre-trade risk controls and position management.

**Risk Checks:**
- Maximum order size per symbol
- Maximum open exposure (position limits)
- Maximum daily loss limit (Redis-backed)
- Message rate throttling
- Price band validation (fat-finger protection)
- Kill-switch (can be activated externally)

**Features:**
- Redis-backed state storage
- Fast reject path
- Real-time position tracking

**Ports:**
- `5558`: Approved orders (output)

### 4. Order Management System (`oms`)

Persistent order state management.

**Features:**
- PostgreSQL order storage with WAL
- Redis low-latency cache
- Order state machine (NEW → ACKNOWLEDGED → FILLED/CANCELLED/REJECTED)
- Unique order ID generation
- Idempotency handling
- Retry & resend logic with exponential backoff
- Audit trail

**Ports:**
- `5559`: Orders to execution (output)

### 5. Execution Engine (`execution`)

Simulated exchange matching engine.

**Features:**
- Order matching logic (limit & market orders)
- Internal order book for pending orders
- Configurable latency simulation (50-500μs)
- Execution reports (ACK, Partial Fill, Fill, Reject)
- Asynchronous order processing

**Ports:**
- `5560`: Execution reports (output)

### 6. Monitoring (`monitoring`)

Real-time observability and metrics.

**Features:**
- Prometheus metrics integration
- HTTP metrics endpoint (`/metrics`)
- Real-time web dashboard
- Metrics tracked:
  - Tick processing latency (min/avg/max)
  - Ticks per second
  - Signals per second
  - Orders per second
  - Fill statistics
  - Active positions
  - Risk status
- Recent trades and orders
- Backtest API

**Endpoints:**
- `GET /dashboard`: Web dashboard
- `GET /metrics`: Prometheus metrics
- `GET /api/positions`: Current positions
- `GET /api/risk/status`: Risk management status
- `POST /api/risk/kill-switch`: Activate/deactivate kill-switch
- `GET /api/trades`: Recent trades
- `GET /api/orders`: Recent orders
- `GET /api/metrics/data`: JSON metrics data
- `GET /api/backtest`: Run backtests

**Ports:**
- `9090`: HTTP server

### 7. Security (`security`)

Authentication and authorization.

**Features:**
- JWT token generation
- Role-based access control (Ops, Risk, Dev, System)
- Token validation
- Permission checking infrastructure

**Ports:**
- `9

## 💻 Development

### Adding a New Strategy

1. Create a new file in `strategies/` directory implementing the `Strategy` trait:

```rust
use shared::*;
use crate::strategy::Strategy;

pub struct MyStrategy {
    name: String,
    // Your strategy state
}

impl Strategy for MyStrategy {
    fn name(&self) -> &str { &self.name }
    
    fn initialize(&mut self, params: HashMap<String, String>) -> Result<()> {
        // Initialize strategy parameters
        Ok(())
    }
    
    fn on_tick(&mut self, tick: &TickEvent) -> Vec<Signal> {
        // Generate signals based on tick data
        vec![]
    }
    
    fn on_order_book(&mut self, book: &OrderBookSnapshot) -> Vec<Signal> {
        // Generate signals based on order book
        vec![]
    }
}
```

2. Register the strategy in `src/main.rs`:

```rust
let mut my_strategy = Box::new(MyStrategy::new("MyStrategy-1".to_string()));
my_strategy.initialize(params)?;
runtime.register_strategy(my_strategy).await;
```

### Building Individual Components

```bash
# Build a specific component
cargo build --release --bin market_data
cargo build --release --bin strategies
cargo build --release --bin risk
cargo build --release --bin oms
cargo build --release --bin execution
cargo build --release --bin monitoring
cargo build --release --bin security

# Run a specific component
cargo run --release --bin market_data
```

### Debugging

Enable debug logging:

```bash
RUST_LOG=debug cargo run --release --bin monitoring
```

Or in PowerShell:
```powershell
$env:RUST_LOG="debug"
cargo run --release --bin monitoring
```

## 🧪 Testing

### Unit Tests

```bash
# Run all tests
cargo test

# Run tests for a specific module
cargo test --lib market_data

# Run with output
cargo test -- --nocapture
```

### Integration Tests

```bash
# Run integration tests (requires Redis and PostgreSQL)
cargo test --test strategy_test
cargo test --test stress_test
```

### Stress Testing

The platform includes stress tests targeting 1M ticks/second processing:

```bash
cargo test --test stress_test --release
```

- `max_tick_latency_us`: Maximum tick latency

### Grafana Integration

To integrate with Grafana:

1. Configure Prometheus to scrape `http://localhost:9090/metrics`
2. Import dashboards using the Prometheus data source
3. Monitor real-time performance metrics

## 🔒 Security Considerations

- **Kill Switch**: Emergency stop mechanism to halt all trading
- **Rate Limiting**: Prevents runaway strategies
- **Position Limits**: Caps exposure per symbol
- **Daily Loss Limits**: Automatic shutdown on excessive losses
- **Price Band Validation**: Prevents fat-finger errors
- **JWT Authentication**: Secure API access (WIP: full implementation)

## 📝 Notes

- The platform currently uses CSV replay for market data. Live exchange feeds can be integrated.
- Python strategy support infrastructure is in place but not fully implemented.
- Shared memory ring-buffers could replace ZeroMQ for even lower latency in production.
- Kafka integration can be added for order replay and debugging.

## 🤝 Contributing

Contributions are welcome! Please ensure:

1. Code follows Rust best practices
2. All tests pass
3. Documentation is updated
4. Changes are tested with the full system

## 📄 License

[Specify your license here]

## 🙏 Acknowledgments

Built with Rust, ZeroMQ, PostgreSQL, Redis, and Prometheus for high-performance algorithmic trading.

---

**Note**: This platform is designed for educational and development purposes. For production use, ensure proper testing, security hardening, and compliance with applicable regulations.

