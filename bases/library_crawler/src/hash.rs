use color_eyre::Result;
use music_facts::ContentHash;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

/// Compute SHA256 hash of file contents
/// 
/// This is the entity ID for tracks in the fact stream
pub fn compute_content_hash(path: &Path) -> Result<ContentHash> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    
    // Read file in chunks and update hash
    let mut buffer = [0u8; 8192];
    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }
    
    // Finalize and convert to hex string
    let result = hasher.finalize();
    let hash_string = hex::encode(result);
    
    Ok(ContentHash(format!("sha256:{}", hash_string)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;
    
    #[test]
    fn hash_consistency() {
        // Create temp file with known content
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(b"test content").unwrap();
        
        // Hash twice
        let hash1 = compute_content_hash(temp_file.path()).unwrap();
        let hash2 = compute_content_hash(temp_file.path()).unwrap();
        
        // Should be identical
        assert_eq!(hash1, hash2);
    }
    
    #[test]
    fn hash_format() {
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(b"test").unwrap();
        
        let hash = compute_content_hash(temp_file.path()).unwrap();
        
        // Should start with "sha256:"
        assert!(hash.0.starts_with("sha256:"));
        
        // Should be 71 chars total ("sha256:" + 64 hex chars)
        assert_eq!(hash.0.len(), 71);
    }
}
