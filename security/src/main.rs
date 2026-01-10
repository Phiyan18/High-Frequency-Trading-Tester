// Security & Safety Layer
// Provides authentication tokens and role-based access control

use shared::auth::{AuthService, Role};
use std::sync::Arc;
use tokio::sync::RwLock;
use axum::{Router, routing::post, Json, extract::State};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
struct TokenRequest {
    subject: String,
    role: String,
    expiry_hours: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize)]
struct TokenResponse {
    token: String,
    expires_in: i64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    // Load secret from environment or use default (in production, use proper secret management)
    let secret = std::env::var("JWT_SECRET")
        .unwrap_or_else(|_| "hft_platform_secret_key_must_be_at_least_32_bytes_long_for_security".to_string());
    
    let auth_service = Arc::new(RwLock::new(AuthService::new(&secret)));

    let app = Router::new()
        .route("/token", post(generate_token))
        .with_state(auth_service);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:9091").await?;
    println!("🔐 Security Service started");
    println!("   Token endpoint: http://localhost:9091/token");
    
    axum::serve(listener, app).await?;

    Ok(())
}

async fn generate_token(
    State(auth_service): State<Arc<RwLock<AuthService>>>,
    Json(request): Json<TokenRequest>,
) -> Json<TokenResponse> {
    let role = match request.role.to_uppercase().as_str() {
        "OPS" => Role::Ops,
        "RISK" => Role::Risk,
        "DEV" => Role::Dev,
        "SYSTEM" => Role::System,
        _ => Role::Dev, // Default to Dev for safety
    };

    let expiry_hours = request.expiry_hours.unwrap_or(24);
    
    let auth = auth_service.read().await;
    let token = auth.generate_token(&request.subject, role, expiry_hours).unwrap();
    
    Json(TokenResponse {
        token,
        expires_in: expiry_hours * 3600,
    })
}

