//! Writing an age file (age-encryption.org/v1) with exactly one recipient.
//!
//! Reading is left to the `age` crate. Writing is done here because that crate adds a second, random "grease"
//! recipient to every file it writes. drand's own tools write none: their JavaScript one cannot read a grease
//! recipient whose body is empty, and older ones refuse any second recipient. A file written here is the plain
//! form every tlock tool reads, and the `age` crate reads it back in the tests.
//!
//! The format: a text header naming the recipient and carrying the wrapped file key, authenticated with
//! HMAC-SHA-256; then a 16-byte nonce; then the data in 64 KiB chunks, each sealed with ChaCha20-Poly1305
//! under a key derived from the file key and the nonce (the STREAM construction, eprint 2015/189).

use age_core::primitives::hkdf;
use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use chacha20poly1305::aead::Aead;
use chacha20poly1305::{ChaCha20Poly1305, KeyInit};
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::{Error, Result};

/// Bytes of a file key.
pub(crate) const KEY_BYTES: usize = 16;
/// Plaintext bytes in every chunk but the last.
const CHUNK_BYTES: usize = 64 * 1024;
/// Columns of a line of a recipient's body.
const COLUMNS: usize = 64;

fn failed(what: &str) -> Error {
    Error::Sealed(format!("sealing failed: {what}"))
}

/// Writes a file to one recipient, in binary form.
///
/// `tag` and `args` name the recipient; `wrap` turns the fresh file key into the recipient's body. None of
/// the three may hold a space or a line break.
pub(crate) fn write(
    tag: &str,
    args: &[&str],
    wrap: impl FnOnce(&[u8; KEY_BYTES]) -> Result<Vec<u8>>,
    data: &[u8],
) -> Result<Vec<u8>> {
    let words = || std::iter::once(&tag).chain(args);
    if words().any(|word| word.is_empty() || !word.bytes().all(|c| c.is_ascii_graphic())) {
        return Err(failed("a recipient is named in plain words"));
    }
    let (mut file_key, mut nonce) = ([0u8; KEY_BYTES], [0u8; 16]);
    getrandom::getrandom(&mut file_key)
        .and_then(|()| getrandom::getrandom(&mut nonce))
        .map_err(|_| failed("the system gave no randomness"))?;

    let mut header = String::from("age-encryption.org/v1\n->");
    for word in words() {
        header.push(' ');
        header.push_str(word);
    }
    header.push('\n');
    // The body in lines of 64 columns. The last line is always shorter: empty when the body fills its lines.
    let body = STANDARD_NO_PAD.encode(wrap(&file_key)?);
    for line in body.as_bytes().chunks(COLUMNS) {
        header.extend(line.iter().map(|&c| char::from(c)));
        header.push('\n');
    }
    if body.len().is_multiple_of(COLUMNS) {
        header.push('\n');
    }
    // The header is authenticated up to and including these dashes.
    header.push_str("---");
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&hkdf(&[], b"header", &file_key))
        .map_err(|_| failed("the header could not be authenticated"))?;
    mac.update(header.as_bytes());
    header.push(' ');
    header.push_str(&STANDARD_NO_PAD.encode(mac.finalize().into_bytes()));
    header.push('\n');

    let cipher = ChaCha20Poly1305::new(&hkdf(&nonce, b"payload", &file_key).into());
    let mut file = header.into_bytes();
    file.extend_from_slice(&nonce);
    let mut chunks = data.chunks(CHUNK_BYTES).peekable();
    for counter in 0u64.. {
        // Empty data is one empty chunk; otherwise the last chunk is the last piece of the data, full or not.
        let chunk = chunks.next().unwrap_or(&[]);
        let last = chunks.peek().is_none();
        // Each chunk has its own nonce: its number in eleven bytes, then one byte that marks the last chunk.
        let mut chunk_nonce = [0u8; 12];
        chunk_nonce[3..11].copy_from_slice(&counter.to_be_bytes());
        chunk_nonce[11] = u8::from(last);
        file.extend(cipher.encrypt(&chunk_nonce.into(), chunk).map_err(|_| failed("a chunk did not encrypt"))?);
        if last {
            break;
        }
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use age_core::format::{FileKey, Stanza};

    use super::*;

    /// A recipient that hands the file key over as it is, to test the file format apart from any wrapping.
    struct Plain;

    impl age::Identity for Plain {
        fn unwrap_stanza(&self, stanza: &Stanza) -> Option<std::result::Result<FileKey, age::DecryptError>> {
            let key: [u8; KEY_BYTES] = stanza.body.as_slice().try_into().ok()?;
            (stanza.tag == "plain").then(|| Ok(FileKey::new(Box::new(key))))
        }
    }

    fn written(data: &[u8]) -> Vec<u8> {
        write("plain", &["one", "two"], |key| Ok(key.to_vec()), data).unwrap()
    }

    fn read_back(file: &[u8]) -> std::result::Result<Vec<u8>, String> {
        let decryptor = age::Decryptor::new(file).map_err(|e| e.to_string())?;
        let mut reader = decryptor.decrypt(std::iter::once(&Plain as &dyn age::Identity)).map_err(|e| e.to_string())?;
        let mut data = Vec::new();
        reader.read_to_end(&mut data).map_err(|e| e.to_string())?;
        Ok(data)
    }

    #[test]
    fn the_age_crate_reads_what_is_written_at_every_chunk_boundary() {
        for size in
            [0, 1, 15, 16, 17, CHUNK_BYTES - 1, CHUNK_BYTES, CHUNK_BYTES + 1, 2 * CHUNK_BYTES, 3 * CHUNK_BYTES + 5]
        {
            let data: Vec<u8> = (0..size).map(|i| (i * 31 % 251) as u8).collect();
            let file = written(&data);
            assert_eq!(read_back(&file).unwrap(), data, "{size} bytes");
            // A chunk is its plaintext and a 16-byte tag; empty data is still one chunk.
            let chunks = size.div_ceil(CHUNK_BYTES).max(1);
            let header = file.windows(5).position(|w| w == b"\n--- ").unwrap() + 5 + 43 + 1;
            assert_eq!(file.len(), header + 16 + size + 16 * chunks, "{size} bytes");
        }
    }

    #[test]
    fn the_header_has_one_recipient_and_nothing_else() {
        let file = written(b"data");
        let end = file.windows(5).position(|w| w == b"\n--- ").unwrap();
        let header = std::str::from_utf8(&file[..end]).unwrap();
        let lines: Vec<&str> = header.lines().collect();
        assert_eq!(lines.len(), 3, "{header}");
        assert_eq!((lines[0], lines[1]), ("age-encryption.org/v1", "-> plain one two"));
        // Sixteen bytes are 22 characters of base64 without padding.
        assert_eq!(lines[2].len(), 22);
        assert_eq!(header.matches("->").count(), 1);
    }

    #[test]
    fn bodies_wrap_at_64_columns_with_a_shorter_last_line() {
        for (bytes, lines) in [
            (0, vec![0]),
            (1, vec![2]),
            (47, vec![63]),
            (48, vec![64, 0]),
            (49, vec![64, 2]),
            (96, vec![64, 64, 0]),
            (128, vec![64, 64, 43]),
        ] {
            let file = write("big", &[], |_| Ok(vec![7u8; bytes]), b"").unwrap();
            let end = file.windows(5).position(|w| w == b"\n--- ").unwrap();
            let header = std::str::from_utf8(&file[..end]).unwrap();
            let body: Vec<usize> = header.split('\n').skip(2).map(str::len).collect();
            assert_eq!(body, lines, "{bytes} bytes");
            // The age crate accepts the header (its recipient is not ours to open).
            assert!(age::Decryptor::new(&file[..]).is_ok(), "{bytes} bytes");
        }
    }

    #[test]
    fn two_files_of_the_same_data_share_no_key_and_no_bytes_of_payload() {
        let (a, b) = (written(&[9u8; 300]), written(&[9u8; 300]));
        let payload = |file: &[u8]| file[file.windows(5).position(|w| w == b"\n--- ").unwrap() + 49..].to_vec();
        let (a, b) = (payload(&a), payload(&b));
        assert_eq!(a.len(), 16 + 300 + 16);
        assert_ne!(a[..16], b[..16], "the nonces differ");
        assert!(a[16..].iter().zip(&b[16..]).filter(|(x, y)| x == y).count() < 40, "the payloads look unrelated");
    }

    #[test]
    fn a_changed_header_or_payload_does_not_read() {
        let file = written(&[3u8; 1000]);
        for at in (0..file.len()).step_by(7) {
            let mut bent = file.clone();
            bent[at] ^= 0x04;
            assert!(read_back(&bent).is_err(), "byte {at}");
        }
        assert!(read_back(&file[..file.len() - 1]).is_err());
    }

    #[test]
    fn names_with_spaces_or_line_breaks_are_refused() {
        for (tag, arg) in
            [("two words", "x"), ("tag", "a b"), ("tag", "a\nb"), ("", "x"), ("tag", ""), ("tag", "caf\u{e9}")]
        {
            assert!(write(tag, &[arg], |key| Ok(key.to_vec()), b"").is_err(), "{tag:?} {arg:?}");
        }
        assert!(write("tag", &[], |_| Err(failed("no")), b"").is_err());
    }
}
