use thiserror::Error;

#[derive(Debug, Error)]
pub enum HttpServerError {
    #[error("TCP bind failed: {source}")]
    Bind {
        #[source]
        source: std::io::Error,
    },
    #[error("server error: {source}")]
    Serve {
        #[source]
        source: std::io::Error,
    },
}

pub struct HttpServerConfig {
    pub port: u16,
}

/// Binds 0.0.0.0:{port} and serves the given Axum router.
/// Does NOT call color_eyre::install() or tracing_subscriber::init().
pub async fn serve(router: axum::Router, config: &HttpServerConfig) -> Result<(), HttpServerError> {
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", config.port))
        .await
        .map_err(|source| HttpServerError::Bind { source })?;
    axum::serve(listener, router)
        .await
        .map_err(|source| HttpServerError::Serve { source })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn bind_port_zero_succeeds() {
        let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await;
        assert!(listener.is_ok());
    }
}
