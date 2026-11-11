use windows_sys::Win32::{
    Foundation::{ERROR_NOT_FOUND, FILETIME, GetLastError},
    Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree,
        CredReadW, CredWriteW,
    },
};

fn target(service: &str, account: &str) -> Vec<u16> {
    format!("{service}/{account}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect()
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn read(service: &str, account: &str) -> Option<String> {
    let target = target(service, account);
    let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: `target` is a valid NUL-terminated UTF-16 string; on success
    // `credential` points to a buffer owned by the system until `CredFree`.
    let found = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) };
    if found == 0 || credential.is_null() {
        return None;
    }
    // SAFETY: the system-owned credential and blob are valid until CredFree.
    let key = unsafe {
        let size = (*credential).CredentialBlobSize as usize;
        let blob = (*credential).CredentialBlob;
        if blob.is_null() || size == 0 {
            None
        } else {
            std::str::from_utf8(std::slice::from_raw_parts(blob, size))
                .ok()
                .map(str::trim)
                .filter(|key| !key.is_empty())
                .map(str::to_string)
        }
    };
    // SAFETY: `credential` came from `CredReadW` and is freed exactly once.
    unsafe { CredFree(credential.cast()) };
    key
}

pub fn write(service: &str, account: &str, key: &str) -> Result<(), u32> {
    let mut target = target(service, account);
    let mut user = wide(account);
    let blob = key.as_bytes();
    let credential = CREDENTIALW {
        Flags: 0,
        Type: CRED_TYPE_GENERIC,
        TargetName: target.as_mut_ptr(),
        Comment: std::ptr::null_mut(),
        LastWritten: FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        },
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_ptr().cast_mut(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        AttributeCount: 0,
        Attributes: std::ptr::null_mut(),
        TargetAlias: std::ptr::null_mut(),
        UserName: user.as_mut_ptr(),
    };
    // SAFETY: every pointer in `credential` refers to a live buffer that
    // outlives this call; the system copies the blob before returning.
    if unsafe { CredWriteW(&credential, 0) } == 0 {
        // SAFETY: reads the calling thread's last error value.
        return Err(unsafe { GetLastError() });
    }
    Ok(())
}

pub fn delete(service: &str, account: &str) -> Result<(), u32> {
    let target = target(service, account);
    // SAFETY: `target` is a valid NUL-terminated UTF-16 string.
    if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } != 0 {
        return Ok(());
    }
    // SAFETY: reads the calling thread's last error value.
    match unsafe { GetLastError() } {
        ERROR_NOT_FOUND => Ok(()),
        error => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_round_trips_and_removal_is_idempotent() {
        let account = format!("tts.doubao.test.{}", std::process::id());
        let service = "com.capwise.test";
        let key = "synthetic-credential";
        write(service, &account, key).unwrap();
        let recovered = read(service, &account);
        delete(service, &account).unwrap();
        assert_eq!(recovered.as_deref(), Some(key));
        assert_eq!(read(service, &account), None);
        delete(service, &account).unwrap();
    }
}
