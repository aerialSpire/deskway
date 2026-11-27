//! Browser trust is authorized by the elevated sign-in and applied by the SYSTEM agent.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use deskway_server_connection::Address;
use windows::core::{w, Error};
use windows::Win32::Foundation::{CRYPT_E_EXISTS, CRYPT_E_NOT_FOUND};
use windows::Win32::Security::Cryptography::*;

use crate::config;

const INSTALLED_ROOT: &str = "installed-ca.der";

fn read_installed(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(der) => Ok(Some(der)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Removes the root added by this Host while the services are stopped.
pub fn remove_installed_server_root(dir: &Path) -> Result<()> {
    let root = config::root_path(dir);
    let trust = root.parent().unwrap();
    if trust.try_exists()? {
        crate::service::install::check_trust_dir(trust)?;
        let installed = trust.join(INSTALLED_ROOT);
        if let Some(der) = read_installed(&installed)? {
            remove_owned(&installed, &der)?;
        }
    }
    Ok(())
}

fn remove_owned(path: &Path, der: &[u8]) -> Result<()> {
    Store::open(false)?
        .remove(der)
        .context("remove the local server root from Windows")?;
    fs::remove_file(path)?;
    Ok(())
}

pub fn sync_server_root(dir: PathBuf, dev: bool) -> Result<()> {
    let path = config::root_path(&dir);
    let trust = path.parent().unwrap();
    if !trust.try_exists()? {
        return Ok(());
    }
    if !dev {
        crate::service::install::check_trust_dir(trust)?;
    }
    let Some(der) = signed_in_root(&dir)? else {
        return Ok(());
    };
    let installed = trust.join(INSTALLED_ROOT);
    let owned = read_installed(&installed)?;
    if let Some(previous) = owned.as_deref() {
        if previous != der {
            remove_owned(&installed, previous)?;
        }
    }
    // Read the logical machine store, including policy roots. Write only to the registry store.
    if Store::open(true)?.contains(&der)? {
        return Ok(());
    }
    let store = Store::open(false)?;
    if store
        .add(&der)
        .context("install the local server root in Windows")?
    {
        if let Err(e) = config::write_private(&installed, &der) {
            store
                .remove(&der)
                .with_context(|| format!("record the installed root: {e:#}"))?;
            return Err(e).context("record the installed root");
        }
    }
    Ok(())
}

fn signed_in_root(dir: &Path) -> Result<Option<Vec<u8>>> {
    let cfg = match config::Config::load_from(dir) {
        Ok(cfg) => cfg,
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == ErrorKind::NotFound) =>
        {
            return Ok(None)
        }
        Err(e) => return Err(e),
    };
    if cfg.id.is_empty() || cfg.token.is_empty() {
        return Ok(None);
    }
    cfg.server_root
        .as_deref()
        .map(|pem| Address::parse(&cfg.server)?.verified_root(pem))
        .transpose()
}

struct Store(HCERTSTORE);

impl Store {
    fn open(read_only: bool) -> Result<Self> {
        let flags =
            CERT_OPEN_STORE_FLAGS(CERT_SYSTEM_STORE_LOCAL_MACHINE) | CERT_STORE_OPEN_EXISTING_FLAG;
        let (provider, flags) = if read_only {
            (CERT_STORE_PROV_SYSTEM_W, flags | CERT_STORE_READONLY_FLAG)
        } else {
            (CERT_STORE_PROV_SYSTEM_REGISTRY_W, flags)
        };
        Ok(Self(unsafe {
            CertOpenStore(
                provider,
                CERT_QUERY_ENCODING_TYPE(0),
                None,
                flags,
                Some(w!("ROOT").0.cast()),
            )
        }?))
    }

    fn add(&self, der: &[u8]) -> Result<bool> {
        match unsafe {
            CertAddEncodedCertificateToStore(
                Some(self.0),
                X509_ASN_ENCODING,
                der,
                CERT_STORE_ADD_NEW,
                None,
            )
        } {
            Ok(()) => Ok(true),
            Err(e) if e.code() == CRYPT_E_EXISTS => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    fn contains(&self, der: &[u8]) -> Result<bool> {
        if let Some(cert) = self.find(der)? {
            unsafe { CertFreeCertificateContext(Some(cert)) }.ok()?;
            return Ok(true);
        }
        Ok(false)
    }

    fn remove(&self, der: &[u8]) -> Result<()> {
        if let Some(cert) = self.find(der)? {
            unsafe { CertDeleteCertificateFromStore(cert) }?;
        }
        Ok(())
    }

    fn find(&self, der: &[u8]) -> Result<Option<*const CERT_CONTEXT>> {
        let mut previous = None;
        loop {
            // Enumeration releases the previous context. The caller releases any match.
            let cert = unsafe { CertEnumCertificatesInStore(self.0, previous) };
            if cert.is_null() {
                let error = Error::from_thread();
                return if error.code() == CRYPT_E_NOT_FOUND {
                    Ok(None)
                } else {
                    Err(error.into())
                };
            }
            let bytes = unsafe {
                std::slice::from_raw_parts((*cert).pbCertEncoded, (*cert).cbCertEncoded as usize)
            };
            if bytes == der {
                return Ok(Some(cert));
            }
            previous = Some(cert);
        }
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = unsafe { CertCloseStore(Some(self.0), 0) };
    }
}
