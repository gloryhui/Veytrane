//! Root compatibility path plus CLI control-message integration coverage.

pub use veytrane_files::manifest::{
    CHUNK_HASH_LEN, ChunkHash, DEFAULT_CHUNK_SIZE, FileManifest, MAX_CHUNK_SIZE, MIN_CHUNK_SIZE,
    chunk_count, chunk_len_at, root_hash_of, safe_file_name, validate_chunk_size,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::message::ControlMessage;

    fn manifest_of(data: &[u8], chunk_size: u32) -> FileManifest {
        let chunks = data
            .chunks(chunk_size as usize)
            .map(ChunkHash::of)
            .collect::<Vec<_>>();
        FileManifest::new("demo.bin", data.len() as u64, chunk_size, chunks).unwrap()
    }

    #[test]
    fn 任意字节输入都不会让解码路径_panic() {
        let mut seed = 0xdead_beef_cafe_1234u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };

        let valid = ControlMessage::Manifest(Box::new(manifest_of(
            &vec![9u8; MIN_CHUNK_SIZE as usize + 3],
            MIN_CHUNK_SIZE,
        )))
        .encode()
        .unwrap();

        for round in 0..3_000u32 {
            let mut bytes = valid.clone();
            if round % 2 == 0 {
                let pos = (next() as usize) % bytes.len();
                bytes[pos] ^= (next() as u8) | 1;
            } else {
                let keep = (next() as usize) % (bytes.len() + 1);
                bytes.truncate(keep);
            }

            let result = std::panic::catch_unwind(|| {
                let _ = ControlMessage::decode(&bytes);
            });
            assert!(result.is_ok(), "第 {round} 轮解码 panic 了");
        }
    }
}
