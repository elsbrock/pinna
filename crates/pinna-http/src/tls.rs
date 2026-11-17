use std::{path::Path, sync::OnceLock};

use rustls::{
    ClientConfig, RootCertStore,
    crypto::ring,
    pki_types::{CertificateDer, pem::PemObject},
};

pub const CA_BUNDLE: &str = "/etc/ssl/certs/ca-certificates.crt";

pub fn tls() -> ClientConfig {
    static CONFIG: OnceLock<ClientConfig> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let path = std::env::var_os("SSL_CERT_FILE").unwrap_or_else(|| CA_BUNDLE.into());
            tls_from(Path::new(&path))
        })
        .clone()
}

pub fn tls_from(bundle: &Path) -> ClientConfig {
    let provider = ring::default_provider();
    let _ = provider.clone().install_default();
    let mut roots = RootCertStore::empty();
    if let Ok(certs) = CertificateDer::pem_file_iter(bundle) {
        roots.add_parsable_certificates(certs.map_while(Result::ok));
    }
    if roots.is_empty() {
        tracing::warn!(path = %bundle.display(), "No trusted certificates loaded. Check the CA bundle.");
    }
    ClientConfig::builder_with_provider(provider.into())
        .with_safe_default_protocol_versions()
        .expect("ring supports TLS")
        .with_root_certificates(roots)
        .with_no_client_auth()
}
