// Rust TUI Dashboard for HFT Algorithm Tester
// Modern dashboard with charts and statistics

use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, Paragraph, Row, Sparkline, Table},
    Frame, Terminal,
};
use std::io;
use std::sync::Arc;
use tokio::sync::RwLock;
use std::collections::VecDeque;

// Import types from main.rs
use crate::{RateTracker, TradeEvent, OrderEvent};

pub(crate) struct DashboardData {
    pub ticks_per_second: f64,
    pub signals_per_second: f64,
    pub orders_per_second: f64,
    pub avg_tick_latency_us: f64,
    pub min_tick_latency_us: f64,
    pub max_tick_latency_us: f64,
    pub total_trades: u64,
    pub total_pnl: f64,
    pub recent_trades: Vec<(String, String, f64, f64, String)>,
    pub recent_orders: Vec<(String, String, f64, Option<f64>, String)>,
    pub latency_history: VecDeque<u64>,
}

pub(crate) async fn run_tui_dashboard(
    rate_tracker: Arc<RwLock<RateTracker>>,
    recent_trades: Arc<RwLock<VecDeque<TradeEvent>>>,
    recent_orders: Arc<RwLock<VecDeque<OrderEvent>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen, crossterm::cursor::Hide)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    
    let mut latency_history = VecDeque::with_capacity(50);
    
    loop {
        // Collect data
        let data = {
            let tracker = rate_tracker.read().await;
            let trades_guard = recent_trades.read().await;
            let orders_guard = recent_orders.read().await;
            
            let (avg, min, max) = tracker.get_latency_stats();
            
            // Update latency history for sparkline
            latency_history.push_back(avg as u64);
            if latency_history.len() > 50 {
                latency_history.pop_front();
            }
            
            let recent_trades_vec: Vec<_> = trades_guard.iter().rev().take(10)
                .map(|t| (
                    t.order_id.clone(),
                    t.symbol.clone(),
                    t.quantity,
                    t.price,
                    t.status.clone(),
                ))
                .collect();
            
            let recent_orders_vec: Vec<_> = orders_guard.iter().rev().take(10)
                .map(|o| (
                    o.order_id.clone(),
                    o.symbol.clone(),
                    o.quantity,
                    o.price,
                    o.status.clone(),
                ))
                .collect();
            
            DashboardData {
                ticks_per_second: tracker.get_ticks_per_second(),
                signals_per_second: tracker.get_signals_per_second(),
                orders_per_second: tracker.get_orders_per_second(),
                avg_tick_latency_us: avg,
                min_tick_latency_us: min,
                max_tick_latency_us: max,
                total_trades: trades_guard.len() as u64,
                total_pnl: 0.0, // Calculate from trades if needed
                recent_trades: recent_trades_vec,
                recent_orders: recent_orders_vec,
                latency_history: latency_history.clone(),
            }
        };
        
        terminal.draw(|f| ui(f, &data))?;
        
        // Check for quit key (q)
        if crossterm::event::poll(std::time::Duration::from_millis(100))? {
            if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
                if key.code == crossterm::event::KeyCode::Char('q') {
                    break;
                }
            }
        }
    }
    
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::cursor::Show
    )?;
    
    Ok(())
}

fn ui(f: &mut Frame, data: &DashboardData) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Title
            Constraint::Min(10),    // Main content
            Constraint::Length(1),  // Footer
        ])
        .split(f.size());

    // Modern title with gradient effect
    let title = Paragraph::new("⚡ HFT Algorithm Tester - Real-Time Dashboard ⚡")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
        );
    f.render_widget(title, chunks[0]);

    // Main content - 3 column layout
    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(35),  // Metrics & Charts
            Constraint::Percentage(32),  // Trades
            Constraint::Percentage(33),  // Orders
        ])
        .split(chunks[1]);

    // Left - Metrics with charts
    render_metrics_with_charts(f, main_chunks[0], data);
    
    // Middle - Recent Trades
    render_trades_table(f, main_chunks[1], data);
    
    // Right - Recent Orders
    render_orders_table(f, main_chunks[2], data);

    // Footer
    let footer = Paragraph::new("Press 'q' to quit | Live updates every 100ms")
        .style(Style::default().fg(Color::DarkGray))
        .alignment(Alignment::Center);
    f.render_widget(footer, chunks[2]);
}

fn render_metrics_with_charts(f: &mut Frame, area: Rect, data: &DashboardData) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Header
            Constraint::Length(8),  // Throughput stats
            Constraint::Length(10), // Latency stats with chart
            Constraint::Min(5),     // Summary stats
        ])
        .split(area);

    // Header
    let header = Paragraph::new("📊 Performance Metrics")
        .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
        );
    f.render_widget(header, chunks[0]);

    // Throughput metrics with gauges
    let throughput_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(chunks[1]);

    let tick_gauge = Gauge::default()
        .block(Block::default().borders(Borders::NONE))
        .gauge_style(Style::default().fg(Color::Green))
        .ratio((data.ticks_per_second / 1000.0).min(1.0))
        .label(format!("Ticks/sec: {:.1}", data.ticks_per_second));
    f.render_widget(tick_gauge, throughput_chunks[0]);

    let signal_gauge = Gauge::default()
        .block(Block::default().borders(Borders::NONE))
        .gauge_style(Style::default().fg(Color::Blue))
        .ratio((data.signals_per_second / 100.0).min(1.0))
        .label(format!("Signals/sec: {:.1}", data.signals_per_second));
    f.render_widget(signal_gauge, throughput_chunks[1]);

    let order_gauge = Gauge::default()
        .block(Block::default().borders(Borders::NONE))
        .gauge_style(Style::default().fg(Color::Magenta))
        .ratio((data.orders_per_second / 100.0).min(1.0))
        .label(format!("Orders/sec: {:.1}", data.orders_per_second));
    f.render_widget(order_gauge, throughput_chunks[2]);

    let throughput_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green))
        .title("Throughput");
    f.render_widget(throughput_block, chunks[1]);

    // Latency metrics with sparkline chart
    let latency_text = vec![
        Line::from(vec![
            Span::styled("Avg: ", Style::default().fg(Color::White)),
            Span::styled(
                format!("{:.2} μs", data.avg_tick_latency_us),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ),
            Span::raw("  Min: "),
            Span::styled(
                format!("{:.2} μs", data.min_tick_latency_us),
                Style::default().fg(Color::Green),
            ),
            Span::raw("  Max: "),
            Span::styled(
                format!("{:.2} μs", data.max_tick_latency_us),
                Style::default().fg(Color::Red),
            ),
        ]),
    ];

    let sparkline_data: Vec<u64> = data.latency_history.iter().copied().collect();
    let sparkline = if !sparkline_data.is_empty() {
        let max = sparkline_data.iter().max().copied().unwrap_or(1);
        Sparkline::default()
            .block(Block::default().borders(Borders::NONE))
            .data(&sparkline_data)
            .style(Style::default().fg(Color::Yellow))
            .max(max.max(1))
    } else {
        Sparkline::default()
            .block(Block::default().borders(Borders::NONE))
            .data(&[])
            .style(Style::default().fg(Color::Yellow))
    };

    let latency_paragraph = Paragraph::new(latency_text)
        .block(Block::default().borders(Borders::NONE));
    
    let latency_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(3),
        ])
        .split(chunks[2]);

    f.render_widget(latency_paragraph, latency_chunks[0]);
    f.render_widget(sparkline, latency_chunks[1]);

    let latency_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title("Latency (Last 50 samples)");
    f.render_widget(latency_block, chunks[2]);

    // Summary statistics
    let summary_text = vec![
        Line::from(vec![
            Span::styled("Total Trades: ", Style::default().fg(Color::White)),
            Span::styled(
                format!("{}", data.total_trades),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("Total P&L: ", Style::default().fg(Color::White)),
            Span::styled(
                format!("${:.2}", data.total_pnl),
                Style::default().fg(if data.total_pnl >= 0.0 { Color::Green } else { Color::Red })
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
    ];
    let summary_block = Paragraph::new(summary_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title("Summary")
        );
    f.render_widget(summary_block, chunks[3]);
}

fn render_trades_table(f: &mut Frame, area: Rect, data: &DashboardData) {
    let trade_rows: Vec<Row> = data.recent_trades.iter().take(15)
        .map(|(id, symbol, qty, price, status)| {
            let status_color = match status.as_str() {
                "Filled" => Color::Green,
                "PartiallyFilled" => Color::Yellow,
                _ => Color::Gray,
            };
            Row::new(vec![
                id[..id.len().min(6)].to_string(),
                symbol.clone(),
                format!("{:.4}", qty),
                format!("${:.2}", price),
            ])
            .style(Style::default().fg(status_color))
        })
        .collect();
    
    let widths = [
        Constraint::Length(6),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Min(10),
    ];
    
    let trade_table = Table::new(trade_rows, widths)
        .header(
            Row::new(vec!["ID", "Symbol", "Qty", "Price"])
                .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Green))
                .title("💹 Recent Trades")
        );
    f.render_widget(trade_table, area);
}

fn render_orders_table(f: &mut Frame, area: Rect, data: &DashboardData) {
    let order_rows: Vec<Row> = data.recent_orders.iter().take(15)
        .map(|(id, symbol, qty, price, status)| {
            let status_color = match status.as_str() {
                "Filled" => Color::Green,
                "Acknowledged" => Color::Blue,
                "Pending" => Color::Yellow,
                "Rejected" => Color::Red,
                _ => Color::Gray,
            };
            Row::new(vec![
                id[..id.len().min(6)].to_string(),
                symbol.clone(),
                format!("{:.4}", qty),
                price.map(|p| format!("${:.2}", p)).unwrap_or_else(|| "MKT".to_string()),
            ])
            .style(Style::default().fg(status_color))
        })
        .collect();
    
    let widths = [
        Constraint::Length(6),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Min(10),
    ];
    
    let order_table = Table::new(order_rows, widths)
        .header(
            Row::new(vec!["ID", "Symbol", "Qty", "Price"])
                .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue))
                .title("📋 Recent Orders")
        );
    f.render_widget(order_table, area);
}
