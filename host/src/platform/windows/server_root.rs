//! Browser trust is authorized by the elevated sign-in and applied by the SYSTEM agent.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use deskway_server_connection::root_der;
use windows::core::{w, Error};
use windows::Win32::Foundation::CRYPT_E_NOT_FOUND;
use windows::Win32::Security::Cryptography::*;
use wire::crypto::{hex, sha256};

use crate::config;

fn read_root(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read_to_string(path) {
        Ok(pem) => Ok(Some(root_der(&pem)?)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Revokes the enrollment's authorization and keeps the certificate available for exact removal.
pub fn retire_server_root(dir: &Path) -> Result<()> {
    let root = config::root_path(dir);
    let trust = root.parent().unwrap();
    if trust.try_exists()? {
        crate::service::install::check_trust_dir(trust)?;
        if let Some(der) = read_root(&root)? {
            fs::rename(
                &root,
                trust.join(format!("retired-{}.pem", hex(&sha256(&der)))),
            )?;
        }
    }
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
    let active = read_root(&path)?;
    let retired = fs::read_dir(trust)?
        .filter_map(|entry| match entry {
            Ok(entry) if entry.file_name().to_string_lossy().starts_with("retired-") => {
                Some(Ok(entry.path()))
            }
            Ok(_) => None,
            Err(e) => Some(Err(e)),
        })
        .collect::<std::io::Result<Vec<_>>>()?;
    if active.is_none() && retired.is_empty() {
        return Ok(());
    }
    let store = Store::open()?;
    for file in retired {
        if let Some(der) = read_root(&file)? {
            if active.as_ref() != Some(&der) {
                store
                    .remove(&der)
                    .context("remove the local server root from Windows")?;
            }
            fs::remove_file(&file)?;
        }
    }
    if let Some(der) = active {
        unsafe {
            CertAddEncodedCertificateToStore(
                Some(store.0),
                X509_ASN_ENCODING,
                &der,
                CERT_STORE_ADD_USE_EXISTING,
                None,
            )
        }
        .context("install the local server root in Windows")?;
    }
    Ok(())
}

struct Store(HCERTSTORE);

impl Store {
    fn open() -> Result<Self> {
        // The writable registry store excludes roots inherited from policy or other stores.
        let flags =
            CERT_OPEN_STORE_FLAGS(CERT_SYSTEM_STORE_LOCAL_MACHINE) | CERT_STORE_OPEN_EXISTING_FLAG;
        Ok(Self(unsafe {
            CertOpenStore(
                CERT_STORE_PROV_SYSTEM_REGISTRY_W,
                CERT_QUERY_ENCODING_TYPE(0),
                None,
                flags,
                Some(w!("ROOT").0.cast()),
            )
        }?))
    }

    fn remove(&self, der: &[u8]) -> Result<()> {
        let mut previous = None;
        loop {
            // Enumeration releases the previous context. Deletion releases the matching one.
            let cert = unsafe { CertEnumCertificatesInStore(self.0, previous) };
            if cert.is_null() {
                let error = Error::from_thread();
                return if error.code() == CRYPT_E_NOT_FOUND {
                    Ok(())
                } else {
                    Err(error.into())
                };
            }
            let bytes = unsafe {
                std::slice::from_raw_parts((*cert).pbCertEncoded, (*cert).cbCertEncoded as usize)
            };
            if bytes == der {
                return Ok(unsafe { CertDeleteCertificateFromStore(cert) }?);
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
