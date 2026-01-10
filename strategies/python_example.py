class Strategy:
    def __init__(self):
        self.position = 0
        self.threshold = 0.01
    
    def initialize(self, params):
        if 'threshold' in params:
            self.threshold = float(params['threshold'])
        print(f"Python strategy initialized with threshold={self.threshold}")
    
    def on_orderbook(self, book):
        signals = []
        
        # Simple spread-based logic
        if book['spread'] > self.threshold and self.position == 0:
            signals.append({
                'type': 'BUY',
                'symbol': book['symbol'],
                'quantity': 1.0,
                'price': None  # Market order
            })
            self.position = 1
        
        return signals