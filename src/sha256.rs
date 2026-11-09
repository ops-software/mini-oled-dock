//! SHA-256 through CNG (`bcrypt.dll`, delay-loaded). Used to verify a staged
//! update against its `.sha256` asset.

// The updater is wired in by later tasks; until then this is unused.
#![allow(dead_code)]

use windows::Win32::Security::Cryptography::{BCryptHash, BCRYPT_SHA256_ALG_HANDLE};

/// SHA-256 of `data`, in one `BCryptHash` call on the SHA-256 pseudo-handle
/// (Windows 10+: no provider to open or close). Blocking on large inputs: never
/// call it on the UI thread with a whole exe.
pub fn sha256(data: &[u8]) -> Result<[u8; 32], String> {
    if u32::try_from(data.len()).is_err() {
        return Err(String::from("BCryptHash: input larger than 4 GiB"));
    }
    let mut out = [0u8; 32];
    // SAFETY: the pseudo-handle needs no open/close; `data` and `out` are live
    // slices whose lengths the wrapper passes along (the length fits in a u32,
    // checked above), and BCryptHash writes at most `out.len()` bytes.
    let status = unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, data, &mut out) };
    if status.0 != 0 {
        return Err(format!("BCryptHash: 0x{:08X}", status.0));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        let hex = |h: [u8; 32]| h.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(hex(sha256(b"abc").unwrap()), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(hex(sha256(b"").unwrap()), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(
            hex(sha256(&vec![0u8; 1 << 20]).unwrap()),
            "30e14955ebf1352266dc2ff8067e68104607e750abb9d3b36582b8af909fcb58"
        );
    }
}
