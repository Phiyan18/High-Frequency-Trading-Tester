#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_market_maker_signals() {
        let mut strategy = SimpleMarketMaker::new("test".to_string());
        strategy.initialize(HashMap::new()).unwrap();

        let book = OrderBookSnapshot {
            symbol: "BTC-USD".to_string(),
            timestamp: 1000000,
            bids: vec![],
            asks: vec![],
            mid_price: 50000.0,
            spread: 1.0,
            imbalance: 0.0,
        };

        let signals = strategy.on_orderbook(&book).await.unwrap();
        assert_eq!(signals.len(), 2);  // Bid and Ask
    }

    #[tokio::test]
    async fn test_mean_reversion() {
        let mut strategy = MeanReversion::new("test".to_string(), 10);
        // Test z-score calculation
    }
}