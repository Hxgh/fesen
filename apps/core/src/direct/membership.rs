use fesen_proto::Peer;
use rustls::{
    DigitallySignedStruct, DistinguishedName, SignatureScheme,
    client::danger::HandshakeSignatureValid,
    crypto::{ring, verify_tls12_signature, verify_tls13_signature},
    pki_types::{CertificateDer, UnixTime},
    server::danger::{ClientCertVerified, ClientCertVerifier},
};
use std::{
    collections::HashMap,
    sync::{Arc, OnceLock, RwLock},
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub(crate) fn crypto_provider() -> Arc<rustls::crypto::CryptoProvider> {
    static PROVIDER: OnceLock<Arc<rustls::crypto::CryptoProvider>> = OnceLock::new();
    PROVIDER
        .get_or_init(|| Arc::new(ring::default_provider()))
        .clone()
}

#[derive(Debug, Clone)]
pub struct Allowed {
    pub certificate: Arc<[u8]>,
    pub cancel: CancellationToken,
}

#[derive(Debug, Default)]
struct Lease {
    peers: HashMap<String, Allowed>,
    by_cert: HashMap<Arc<[u8]>, String>,
    expires: Option<Instant>,
}

#[derive(Debug, Default)]
pub struct Membership(RwLock<Lease>);

impl Membership {
    pub fn renew(&self, peers: Vec<Peer>) {
        let mut lease = self.0.write().unwrap();
        let mut old = std::mem::take(&mut lease.peers);
        lease.by_cert.clear();
        lease.peers.reserve(peers.len());
        lease.by_cert.reserve(peers.len());
        for p in peers {
            let prior = old.remove(&p.id);
            let valid = match prior {
                Some(v)
                    if v.certificate.as_ref() == p.certificate.as_slice()
                        && !v.cancel.is_cancelled() =>
                {
                    v
                }
                stale => {
                    if let Some(v) = stale {
                        v.cancel.cancel();
                    }
                    Allowed {
                        certificate: Arc::from(p.certificate),
                        cancel: CancellationToken::new(),
                    }
                }
            };
            lease.by_cert.insert(valid.certificate.clone(), p.id.clone());
            lease.peers.insert(p.id, valid);
        }
        lease.expires = Some(Instant::now() + fesen_proto::LEASE);
        drop(lease);
        for p in old.into_values() {
            p.cancel.cancel();
        }
    }

    pub fn invalidate(&self) {
        let mut lease = self.0.write().unwrap();
        lease.expires = None;
        lease.by_cert.clear();
        for p in lease.peers.drain().map(|(_, p)| p) {
            p.cancel.cancel();
        }
    }

    pub fn is_fresh(&self) -> bool {
        self.0
            .read()
            .unwrap()
            .expires
            .is_some_and(|expires| Instant::now() < expires)
    }

    pub fn peer(&self, peer: &str) -> Option<Allowed> {
        let lease = self.0.read().unwrap();
        lease.expires.filter(|until| Instant::now() < *until)?;
        lease.peers.get(peer).cloned()
    }

    pub fn certificate(&self, cert: &[u8]) -> Option<Allowed> {
        let lease = self.0.read().unwrap();
        lease.expires.filter(|until| Instant::now() < *until)?;
        let id = lease.by_cert.get(cert)?;
        lease.peers.get(id).cloned()
    }

    #[cfg(test)]
    pub(super) fn expire_now(&self) {
        self.0.write().unwrap().expires = Some(Instant::now());
    }
}

impl ClientCertVerifier for Membership {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        leaf: &CertificateDer<'_>,
        chain: &[CertificateDer<'_>],
        _: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        if chain.is_empty() && self.certificate(leaf.as_ref()).is_some() {
            Ok(ClientCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::UnknownIssuer,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        msg: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            msg,
            cert,
            dss,
            &crypto_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        msg: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            msg,
            cert,
            dss,
            &crypto_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        static SCHEMES: OnceLock<Vec<SignatureScheme>> = OnceLock::new();
        SCHEMES
            .get_or_init(|| {
                crypto_provider()
                    .signature_verification_algorithms
                    .supported_schemes()
            })
            .clone()
    }
}
