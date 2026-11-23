use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use tokio::sync::{Semaphore, mpsc};

struct TestKey {
    unwraps: Arc<AtomicUsize>,
    pending: Option<(mpsc::Sender<()>, Arc<Semaphore>)>,
}

#[async_trait::async_trait]
impl Key for TestKey {
    async fn wrap(&self, key: &[u8; 32]) -> Result<(String, Vec<u8>), Error> {
        Ok(("fixture-key".into(), key.to_vec()))
    }

    async fn unwrap(&self, _id: &str, wrapped: &[u8]) -> Result<Zeroizing<[u8; 32]>, Error> {
        self.unwraps.fetch_add(1, Ordering::Relaxed);
        if let Some((started, resume)) = &self.pending {
            assert_eq!(wrapped, b"slow");
            started.send(()).await.unwrap();
            let _permit = resume.acquire().await.unwrap();
            return Ok(Zeroizing::new([9; 32]));
        }
        Ok(Zeroizing::new(wrapped.try_into().unwrap()))
    }
}

#[tokio::test]
async fn data_key_cache_reuses_unwraps_and_evicts_the_least_recently_used() {
    let unwraps = Arc::new(AtomicUsize::new(0));
    let envelope = Envelope::new(Box::new(TestKey {
        unwraps: Arc::clone(&unwraps),
        pending: None,
    }));
    let mut rows = Vec::new();
    for _ in 0..CACHE_ENTRIES {
        let row = envelope.encrypt(b"token").await.unwrap();
        envelope.decrypt(&row).await.unwrap();
        rows.push(row);
    }
    assert_eq!(unwraps.load(Ordering::Relaxed), CACHE_ENTRIES);
    envelope.decrypt(&rows[0]).await.unwrap();
    assert_eq!(unwraps.load(Ordering::Relaxed), CACHE_ENTRIES);
    let extra = envelope.encrypt(b"token").await.unwrap();
    envelope.decrypt(&extra).await.unwrap();
    assert_eq!(envelope.cache.lock().await.len(), CACHE_ENTRIES);
    envelope.decrypt(&rows[0]).await.unwrap();
    assert_eq!(unwraps.load(Ordering::Relaxed), CACHE_ENTRIES + 1);
    envelope.decrypt(&rows[1]).await.unwrap();
    assert_eq!(unwraps.load(Ordering::Relaxed), CACHE_ENTRIES + 2);
}

#[tokio::test]
async fn slow_unwrap_allows_cached_decrypt_and_keeps_cache_bounded() {
    let key_id = "fixture-key".to_owned();
    let data_key = [9; 32];
    let (nonce, ciphertext) = seal(&data_key, b"private-token").unwrap();
    let cached_row = Encrypted {
        nonce: nonce.clone(),
        ciphertext: ciphertext.clone(),
        key_id: key_id.clone(),
        wrapped_key: 0_usize.to_be_bytes().to_vec(),
    };
    let slow_row = Encrypted {
        nonce,
        ciphertext,
        key_id: key_id.clone(),
        wrapped_key: b"slow".to_vec(),
    };
    let cache = (0..CACHE_ENTRIES)
        .map(|index| CachedKey {
            id: key_id.clone(),
            wrapped: index.to_be_bytes().to_vec(),
            key: Zeroizing::new(data_key),
        })
        .collect();
    let (started, mut pending) = tokio::sync::mpsc::channel(2);
    let resume = Arc::new(Semaphore::new(0));
    let envelope = Arc::new(Envelope {
        key: Box::new(TestKey {
            unwraps: Arc::new(AtomicUsize::new(0)),
            pending: Some((started, Arc::clone(&resume))),
        }),
        cache: Mutex::new(cache),
    });
    let first = {
        let envelope = Arc::clone(&envelope);
        let row = slow_row.clone();
        tokio::spawn(async move { envelope.decrypt(&row).await })
    };
    tokio::time::timeout(Duration::from_secs(5), pending.recv())
        .await
        .unwrap()
        .unwrap();
    let plaintext = tokio::time::timeout(Duration::from_secs(5), envelope.decrypt(&cached_row))
        .await
        .expect("a stalled unwrap must not block a cached decrypt")
        .unwrap();
    assert_eq!(plaintext.expose(), b"private-token");
    let second = {
        let envelope = Arc::clone(&envelope);
        let row = slow_row.clone();
        tokio::spawn(async move { envelope.decrypt(&row).await })
    };
    tokio::time::timeout(Duration::from_secs(5), pending.recv())
        .await
        .unwrap()
        .unwrap();
    resume.add_permits(2);
    for pending in [first, second] {
        assert_eq!(pending.await.unwrap().unwrap().expose(), b"private-token");
    }
    let cache = envelope.cache.lock().await;
    assert_eq!(cache.len(), CACHE_ENTRIES);
    assert_eq!(
        cache
            .iter()
            .filter(|entry| entry.id == slow_row.key_id && entry.wrapped == slow_row.wrapped_key)
            .count(),
        1
    );
    assert!(
        cache
            .iter()
            .any(|entry| entry.wrapped == cached_row.wrapped_key)
    );
}
