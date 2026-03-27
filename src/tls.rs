use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Arc;
use tokio_postgres_rustls::MakeRustlsConnect;
use tokio_rustls::TlsAcceptor;

/// TLS mode for PostgreSQL connections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsMode {
    /// No TLS (plaintext).
    Disable,
    /// Use TLS if server supports it, fall back to plaintext.
    Prefer,
    /// Require TLS, but don't verify server certificate.
    Require,
}

/// Build a TLS connector based on mode and optional CA cert.
pub fn make_tls_connector(
    mode: TlsMode,
    ca_cert_path: Option<&Path>,
) -> Result<Option<MakeRustlsConnect>> {
    match mode {
        TlsMode::Disable => Ok(None),
        TlsMode::Prefer | TlsMode::Require => {
            let config = if let Some(ca_path) = ca_cert_path {
                let cert_pem = std::fs::read(ca_path)
                    .with_context(|| format!("Failed to read CA cert: {}", ca_path.display()))?;
                let mut reader = std::io::BufReader::new(&cert_pem[..]);
                let certs: Vec<_> =
                    rustls_pemfile::certs(&mut reader).collect::<std::result::Result<_, _>>()?;
                let mut root_store = rustls::RootCertStore::empty();
                for cert in certs {
                    root_store.add(cert)?;
                }
                rustls::ClientConfig::builder()
                    .with_root_certificates(root_store)
                    .with_no_client_auth()
            } else {
                // Use system CA certificates
                let mut root_store = rustls::RootCertStore::empty();
                let native_certs = rustls_native_certs::load_native_certs();
                for cert in native_certs.certs {
                    let _ = root_store.add(cert);
                }
                rustls::ClientConfig::builder()
                    .with_root_certificates(root_store)
                    .with_no_client_auth()
            };
            Ok(Some(MakeRustlsConnect::new(config)))
        }
    }
}

/// Parse a TLS mode string from CLI.
pub fn parse_tls_mode(s: &str) -> Result<TlsMode> {
    match s.to_lowercase().as_str() {
        "disable" | "off" | "no" => Ok(TlsMode::Disable),
        "prefer" => Ok(TlsMode::Prefer),
        "require" => Ok(TlsMode::Require),
        other => anyhow::bail!(
            "Unknown TLS mode '{}'. Use: disable, prefer, require",
            other
        ),
    }
}

/// Build a TLS acceptor for client-facing TLS (proxy server-side).
///
/// Reads PEM-encoded certificate chain and private key from the given paths
/// and constructs a `TlsAcceptor` for use in the proxy accept loop.
pub fn build_tls_acceptor(cert_path: &Path, key_path: &Path) -> Result<TlsAcceptor> {
    let cert_pem = std::fs::read(cert_path)
        .with_context(|| format!("Failed to read TLS cert: {}", cert_path.display()))?;
    let key_pem = std::fs::read(key_path)
        .with_context(|| format!("Failed to read TLS key: {}", key_path.display()))?;

    let certs: Vec<_> = rustls_pemfile::certs(&mut &cert_pem[..])
        .collect::<std::result::Result<_, _>>()
        .context("Failed to parse TLS certificate PEM")?;
    let key = rustls_pemfile::private_key(&mut &key_pem[..])
        .context("Failed to parse TLS private key PEM")?
        .ok_or_else(|| anyhow::anyhow!("No private key found in {}", key_path.display()))?;

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("Failed to build TLS server config")?;

    Ok(TlsAcceptor::from(Arc::new(config)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tls_mode() {
        assert_eq!(parse_tls_mode("disable").unwrap(), TlsMode::Disable);
        assert_eq!(parse_tls_mode("prefer").unwrap(), TlsMode::Prefer);
        assert_eq!(parse_tls_mode("require").unwrap(), TlsMode::Require);
        assert_eq!(parse_tls_mode("off").unwrap(), TlsMode::Disable);
        assert!(parse_tls_mode("invalid").is_err());
    }

    #[test]
    fn test_disable_returns_none() {
        let connector = make_tls_connector(TlsMode::Disable, None).unwrap();
        assert!(connector.is_none());
    }

    #[test]
    fn test_bad_ca_path_errors() {
        let result = make_tls_connector(TlsMode::Require, Some(Path::new("/nonexistent/ca.pem")));
        assert!(result.is_err());
    }

    #[test]
    fn test_build_tls_acceptor_bad_cert_path() {
        let result = build_tls_acceptor(
            Path::new("/nonexistent/cert.pem"),
            Path::new("/nonexistent/key.pem"),
        );
        let err = result.err().expect("should fail");
        assert!(
            format!("{:#}", err).contains("Failed to read TLS cert"),
            "should report cert read failure, got: {:#}",
            err
        );
    }

    #[test]
    fn test_build_tls_acceptor_bad_key_path() {
        // Create a valid-looking cert file but bad key path
        let dir = tempfile::tempdir().unwrap();
        let cert_path = dir.path().join("cert.pem");
        std::fs::write(&cert_path, "not a cert").unwrap();
        let result = build_tls_acceptor(&cert_path, Path::new("/nonexistent/key.pem"));
        let err = result.err().expect("should fail");
        assert!(
            format!("{:#}", err).contains("Failed to read TLS key"),
            "should report key read failure, got: {:#}",
            err
        );
    }

    #[test]
    fn test_build_tls_acceptor_valid_self_signed() {
        // Uses pre-generated self-signed test cert/key from fixtures
        let cert_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/test-cert.pem");
        let key_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/test-key.pem");

        let result = build_tls_acceptor(&cert_path, &key_path);
        assert!(result.is_ok(), "should build acceptor: {:?}", result.err());
    }

    #[test]
    fn test_build_tls_acceptor_empty_key_file() {
        let dir = tempfile::tempdir().unwrap();
        let cert_path = dir.path().join("cert.pem");
        let key_path = dir.path().join("key.pem");
        std::fs::write(&cert_path, "").unwrap();
        std::fs::write(&key_path, "").unwrap();
        let result = build_tls_acceptor(&cert_path, &key_path);
        assert!(result.is_err(), "empty files should fail");
    }
}
